<script lang="ts">
    import { beforeNavigate } from "$app/navigation";
    import { auth } from "$lib/auth.svelte";
    import { invoke, convertFileSrc } from "@tauri-apps/api/core";
    import { listen, type UnlistenFn } from "@tauri-apps/api/event";
    import { open as openDialog } from "@tauri-apps/plugin-dialog";

    type ScanImage = {
        source_path: string;
        filename: string;
        ext: string;
        preview_path: string | null;
        size: number;
        error: string | null;
    };
    type ScanResult = { folder: string; images: ScanImage[] };

    type Meta = {
        title: string;
        original_date: string;
        edtf_date: string;
        source_url: string;
        description: string;
        creator: string;
        reference_id: string;
        license_name: string;
    };

    type Item = ScanImage & { enabled: boolean; meta: Meta };

    type BulkField = { key: keyof Meta; label: string };

    // Fields worth offering "apply to all" for — shared across a folder often
    // enough to be a real time-saver. `title` and `reference_id` are omitted:
    // they're inherently per-image.
    const BULK_FIELDS: Record<string, BulkField> = {
        original_date: { key: "original_date", label: "Original date" },
        edtf_date: { key: "edtf_date", label: "EDTF date" },
        creator: { key: "creator", label: "Creator" },
        source_url: { key: "source_url", label: "Source URL" },
        license_name: { key: "license_name", label: "License" },
        description: { key: "description", label: "Description" },
    };

    function emptyMeta(): Meta {
        return {
            title: "",
            original_date: "",
            edtf_date: "",
            source_url: "",
            description: "",
            creator: "",
            reference_id: "",
            license_name: "",
        };
    }

    function isComplete(m: Meta): boolean {
        return (
            m.title.trim() !== "" &&
            m.original_date.trim() !== "" &&
            m.edtf_date.trim() !== ""
        );
    }

    function displayVal(v: unknown): string {
        const s = String(v ?? "").trim();
        return s === "" ? "(empty)" : s;
    }

    // Empty means "no value yet": an empty target is filled silently, while a
    // non-empty one that differs is a conflict the user must resolve.
    function isEmptyVal(v: unknown): boolean {
        return String(v ?? "").trim() === "";
    }

    function setMetaField(meta: Meta, key: keyof Meta, value: unknown) {
        (meta as Record<string, unknown>)[key] = value;
    }

    let licenses = $state<string[]>([]);

    let folder = $state<string | null>(null);
    let scanning = $state(false);
    let scanError = $state("");
    let scanProgress = $state<{ done: number; total: number } | null>(null);

    let items = $state<Item[]>([]);
    let selectedPath = $state<string | null>(null);

    let dragIndex = $state<number | null>(null);
    let dragOverIndex = $state<number | null>(null);
    // Our own drag preview: a small, semi-transparent thumbnail that follows
    // the cursor. We render this instead of the native drag image, which on
    // WebKitGTK can't rasterize the row and shows nothing at all.
    let dragPreview = $state<{ src: string | null; x: number; y: number } | null>(
        null,
    );

    let bulkNote = $state("");
    let conflict = $state<null | {
        field: BulkField;
        value: unknown;
        empties: Item[];
        conflicts: Item[];
        checked: Record<string, boolean>;
    }>(null);

    let saving = $state(false);
    let saveError = $state("");
    let saveProgress = $state<{
        done: number;
        total: number;
        filename: string;
    } | null>(null);
    let saveResult = $state<{ dest: string; written: number } | null>(null);

    let selected = $derived(
        items.find((i) => i.source_path === selectedPath) ?? null,
    );
    let enabledItems = $derived(items.filter((i) => i.enabled));
    let completeCount = $derived(
        enabledItems.filter((i) => isComplete(i.meta)).length,
    );
    let incompleteCount = $derived(enabledItems.length - completeCount);
    let disabledCount = $derived(items.length - enabledItems.length);
    let canSave = $derived(
        enabledItems.length > 0 && incompleteCount === 0 && !saving,
    );
    let dirty = $derived(items.length > 0 && !saveResult);

    // source_path -> assigned "000001" stem, in enabled order.
    let numbering = $derived.by(() => {
        const m = new Map<string, string>();
        let n = 0;
        for (const i of items) {
            if (i.enabled) {
                n++;
                m.set(i.source_path, String(n).padStart(6, "0"));
            }
        }
        return m;
    });

    function authHeaders(): HeadersInit {
        return auth.accessToken
            ? { Authorization: `Bearer ${auth.accessToken}` }
            : {};
    }

    $effect(() => {
        if (!auth.hydrated) return;
        // Preparing a folder is a local, offline activity — no login required.
        // When connected to an instance, load its licenses so the dropdown is
        // populated; otherwise it simply stays empty.
        if (auth.instanceUrl) void loadLicenses();
    });

    async function loadLicenses() {
        try {
            const resp = await fetch(`${auth.instanceUrl}/api/v2/licenses/`, {
                headers: authHeaders(),
            });
            if (!resp.ok) return;
            const data = await resp.json();
            const arr = Array.isArray(data) ? data : (data.results ?? []);
            licenses = arr
                .map((l: { name?: string }) => l.name)
                .filter((n: unknown): n is string => typeof n === "string");
        } catch {
            // Non-fatal: the license dropdown just stays empty.
        }
    }

    // Progress event wiring for the two long-running native commands.
    $effect(() => {
        let cancelled = false;
        const listeners: UnlistenFn[] = [];
        (async () => {
            const register = async (p: Promise<UnlistenFn>) => {
                const un = await p;
                if (cancelled) un();
                else listeners.push(un);
            };
            await register(
                listen<{ done: number; total: number }>(
                    "prepare-scan-progress",
                    (ev) => {
                        scanProgress = ev.payload;
                    },
                ),
            );
            await register(
                listen<{ done: number; total: number; filename: string }>(
                    "prepare-save-progress",
                    (ev) => {
                        saveProgress = ev.payload;
                    },
                ),
            );
        })();
        return () => {
            cancelled = true;
            listeners.forEach((un) => un());
        };
    });

    // Warn before losing unsaved work — there's no draft persistence.
    beforeNavigate((nav) => {
        if (dirty && !saving) {
            const ok = confirm(
                "Your prepared images and metadata aren't saved and will be lost. Leave anyway?",
            );
            if (!ok) nav.cancel();
        }
    });

    $effect(() => {
        const handler = (e: BeforeUnloadEvent) => {
            if (dirty && !saving) {
                e.preventDefault();
                e.returnValue = "";
            }
        };
        window.addEventListener("beforeunload", handler);
        return () => window.removeEventListener("beforeunload", handler);
    });

    async function pickFolder() {
        if (dirty) {
            const ok = confirm(
                "Choosing a new folder discards your current work here. Continue?",
            );
            if (!ok) return;
        }
        const chosen = await openDialog({
            directory: true,
            multiple: false,
            title: "Choose a folder of images to prepare",
        });
        if (typeof chosen === "string") {
            folder = chosen;
            await scan();
        }
    }

    async function scan() {
        if (!folder) return;
        scanning = true;
        scanError = "";
        scanProgress = null;
        saveResult = null;
        saveError = "";
        bulkNote = "";
        try {
            const res = await invoke<ScanResult>("scan_prepare_folder", {
                folder,
            });
            items = res.images.map((img) => ({
                ...img,
                enabled: true,
                meta: emptyMeta(),
            }));
            selectedPath = items.length ? items[0].source_path : null;
            if (items.length === 0) {
                scanError = "No supported images found in that folder.";
            }
        } catch (e) {
            scanError = String(e);
            items = [];
            selectedPath = null;
        } finally {
            scanning = false;
            scanProgress = null;
        }
    }

    function select(path: string) {
        selectedPath = path;
        bulkNote = "";
    }

    function move(from: number, to: number) {
        if (to < 0 || to >= items.length || from === to) return;
        const next = [...items];
        const [it] = next.splice(from, 1);
        next.splice(to, 0, it);
        items = next;
    }

    // Transparent 1×1 GIF used to blank the native drag ghost. setDragImage IS
    // honored on WebKitGTK (it replaces the default snapshot) — it just can't
    // render a real element — so pointing it at a transparent pixel reliably
    // hides it, leaving our own `dragPreview` as the only visible feedback.
    const blankDragImage =
        typeof Image !== "undefined"
            ? Object.assign(new Image(), {
                  src: "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7",
              })
            : null;

    // The layout's `main` has `will-change: transform`, which makes it the
    // containing block for `position: fixed` — so a ghost rendered inside it is
    // offset by main's position and drifts with scroll (it ends up floating
    // far from the cursor). Portaling it to <body> keeps it viewport-fixed, so
    // clientX/clientY map straight to left/top.
    function portal(node: HTMLElement) {
        document.body.appendChild(node);
        return {
            destroy() {
                node.remove();
            },
        };
    }

    function onDragStart(e: DragEvent, i: number) {
        dragIndex = i;
        if (!e.dataTransfer) return;
        // Populate dataTransfer — some webview engines won't start a drag
        // (so `drop` never fires) unless the drag carries data.
        e.dataTransfer.effectAllowed = "move";
        e.dataTransfer.setData("text/plain", String(i));
        if (blankDragImage) e.dataTransfer.setDragImage(blankDragImage, 0, 0);

        const item = items[i];
        dragPreview = {
            src: item.preview_path ? convertFileSrc(item.preview_path) : null,
            x: e.clientX,
            y: e.clientY,
        };
        // Track the pointer everywhere (not just over rows) so the preview
        // keeps following even past the ends of the list.
        window.addEventListener("dragover", onWindowDragOver);
    }
    function onWindowDragOver(e: DragEvent) {
        if (dragPreview) {
            dragPreview.x = e.clientX;
            dragPreview.y = e.clientY;
        }
    }
    function endDrag() {
        dragIndex = null;
        dragOverIndex = null;
        dragPreview = null;
        window.removeEventListener("dragover", onWindowDragOver);
    }
    function onDragOver(e: DragEvent, i: number) {
        e.preventDefault();
        if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
        dragOverIndex = i;
    }
    function onDrop(e: DragEvent, i: number) {
        e.preventDefault();
        if (dragIndex !== null) move(dragIndex, i);
        endDrag();
    }
    function onDragEnd() {
        endDrag();
    }

    function applyToAll(field: BulkField) {
        if (!selected) return;
        const { key } = field;
        const value = selected.meta[key];
        const others = items.filter(
            (i) => i.enabled && i.source_path !== selected!.source_path,
        );
        const empties = others.filter((i) => isEmptyVal(i.meta[key]));
        const conflicts = others.filter(
            (i) => !isEmptyVal(i.meta[key]) && i.meta[key] !== value,
        );

        if (conflicts.length === 0) {
            for (const i of empties) setMetaField(i.meta, key, value);
            bulkNote = empties.length
                ? `Applied ${field.label} to ${empties.length} image${empties.length === 1 ? "" : "s"}.`
                : `No other enabled images needed ${field.label}.`;
            return;
        }

        const checked: Record<string, boolean> = {};
        for (const c of conflicts) checked[c.source_path] = false;
        conflict = { field, value, empties, conflicts, checked };
    }

    function confirmBulk() {
        if (!conflict) return;
        const { field, value, empties, conflicts, checked } = conflict;
        for (const i of empties) setMetaField(i.meta, field.key, value);
        let overwritten = 0;
        for (const i of conflicts) {
            if (checked[i.source_path]) {
                setMetaField(i.meta, field.key, value);
                overwritten++;
            }
        }
        const n = empties.length + overwritten;
        bulkNote = `Applied ${field.label} to ${n} image${n === 1 ? "" : "s"}.`;
        conflict = null;
    }

    function setAllConflicts(v: boolean) {
        if (!conflict) return;
        for (const c of conflict.conflicts) conflict.checked[c.source_path] = v;
    }

    async function pickDestAndSave() {
        if (!canSave) return;
        const dest = await openDialog({
            directory: true,
            multiple: false,
            title: "Choose an empty destination folder",
        });
        if (typeof dest !== "string") return;

        saving = true;
        saveError = "";
        saveProgress = null;
        saveResult = null;
        try {
            const payload = items
                .filter((i) => i.enabled)
                .map((i) => ({
                    source_path: i.source_path,
                    sidecar: {
                        title: i.meta.title,
                        original_date: i.meta.original_date,
                        edtf_date: i.meta.edtf_date,
                        source_url: i.meta.source_url,
                        description: i.meta.description,
                        creator: i.meta.creator,
                        reference_id: i.meta.reference_id,
                        license_name: i.meta.license_name,
                        // Not editable in this tool; the import schema still
                        // requires them, so write the no-transform defaults.
                        rotation: 0,
                        mirror: "none",
                    },
                }));
            saveResult = await invoke<{ dest: string; written: number }>(
                "save_prepare_folder",
                { dest, items: payload },
            );
        } catch (e) {
            saveError = String(e);
        } finally {
            saving = false;
            saveProgress = null;
        }
    }
</script>

<nav aria-label="breadcrumb">
    <ol class="breadcrumb">
        <li class="breadcrumb-item"><a href="/">Home</a></li>
        <li class="breadcrumb-item active" aria-current="page">
            Prepare import folder
        </li>
    </ol>
</nav>

{#if !auth.hydrated}
    <div class="d-flex align-items-center text-body-secondary">
        <span
            class="spinner-border spinner-border-sm me-2"
            role="status"
            aria-hidden="true"
        ></span>
        Loading…
    </div>
{:else}
    <h1 class="mb-2">Prepare import folder</h1>
    <p class="text-body-secondary mb-3">
        Build an import-ready folder from a folder of images. Reorder, exclude,
        and fill in each image's metadata, then save a numbered
        <code>000001.json</code> + image series you can import into any
        collection.
    </p>

    <div class="alert alert-warning small" role="alert">
        <strong>This workspace isn't saved.</strong> Your ordering and metadata
        live only on this screen — if you close the app or leave this page before
        saving to a folder, they'll be lost. Originals are only ever copied,
        never changed.
    </div>

    <!-- Step 1: choose folder -->
    <div class="card mb-4">
        <div class="card-body">
            <h2 class="h5 card-title">1. Choose folder</h2>
            <p class="card-text text-body-secondary small mb-3">
                Supported: JPEG, PNG, WebP, TIFF. Files are listed in natural
                order; you can rearrange them below.
            </p>
            <button
                type="button"
                class="btn btn-outline-primary"
                disabled={scanning || saving}
                onclick={pickFolder}
            >
                {folder ? "Change folder…" : "Choose folder…"}
            </button>

            {#if folder}
                <div class="small text-body-secondary text-break mt-3 mb-0">
                    <code>{folder}</code>
                </div>
            {/if}

            {#if scanning}
                <div class="d-flex align-items-center small text-body-secondary mt-3">
                    <span
                        class="spinner-border spinner-border-sm me-2"
                        role="status"
                        aria-hidden="true"
                    ></span>
                    {#if scanProgress}
                        Rendering previews… {scanProgress.done}/{scanProgress.total}
                    {:else}
                        Scanning…
                    {/if}
                </div>
            {/if}

            {#if scanError}
                <div class="alert alert-danger small mt-3 mb-0" role="alert">
                    {scanError}
                </div>
            {/if}
        </div>
    </div>

    {#if items.length > 0}
        <!-- Summary + save -->
        <div
            class="d-flex flex-wrap align-items-center justify-content-between gap-2 mb-3"
        >
            <div class="small text-body-secondary">
                <strong>{items.length}</strong> total ·
                <strong>{enabledItems.length}</strong> included ·
                <strong class="text-success">{completeCount}</strong> complete ·
                <strong class:text-warning={incompleteCount > 0}
                    >{incompleteCount}</strong
                > incomplete ·
                <strong>{disabledCount}</strong> excluded
            </div>
            <button
                type="button"
                class="btn btn-primary"
                disabled={!canSave}
                onclick={pickDestAndSave}
            >
                {#if saving}
                    <span
                        class="spinner-border spinner-border-sm me-1"
                        role="status"
                        aria-hidden="true"
                    ></span>
                    Saving…
                {:else}
                    Save to folder…
                {/if}
            </button>
        </div>

        {#if incompleteCount > 0}
            <div class="alert alert-warning small py-2" role="alert">
                {incompleteCount} included image{incompleteCount === 1
                    ? " is"
                    : "s are"} missing required metadata (title, original date,
                and EDTF date). Complete or exclude them to save.
            </div>
        {/if}

        {#if saving && saveProgress}
            <div class="progress mb-3" role="progressbar">
                <div
                    class="progress-bar progress-bar-striped progress-bar-animated"
                    style="width: {Math.round(
                        (saveProgress.done / saveProgress.total) * 100,
                    )}%"
                >
                    {saveProgress.done}/{saveProgress.total}
                </div>
            </div>
        {/if}

        {#if saveError}
            <div class="alert alert-danger small" role="alert">{saveError}</div>
        {/if}

        {#if saveResult}
            <div class="alert alert-success small" role="alert">
                <strong
                    >Saved {saveResult.written} image{saveResult.written === 1
                        ? ""
                        : "s"}.</strong
                >
                <div class="text-break mt-1"><code>{saveResult.dest}</code></div>
                <div class="mt-2">
                    Open the collection you want to import into, choose
                    <strong>Import images</strong>, and select this folder.
                </div>
                <div class="mt-2">
                    <a class="btn btn-sm btn-outline-success" href="/browse">
                        Browse {auth.instanceName} →
                    </a>
                </div>
            </div>
        {/if}

        <div class="row g-3">
            <!-- Left: ordered list -->
            <div class="col-lg-6">
                <div class="list-group prep-list">
                    {#each items as item, i (item.source_path)}
                        <div
                            class="prep-row"
                            class:selected={item.source_path === selectedPath}
                            class:excluded={!item.enabled}
                            class:drag-over={dragOverIndex === i}
                            draggable="true"
                            role="button"
                            tabindex="0"
                            ondragstart={(e) => onDragStart(e, i)}
                            ondragover={(e) => onDragOver(e, i)}
                            ondrop={(e) => onDrop(e, i)}
                            ondragend={onDragEnd}
                            onclick={() => select(item.source_path)}
                            onkeydown={(e) => {
                                if (e.key === "Enter" || e.key === " ") {
                                    e.preventDefault();
                                    select(item.source_path);
                                }
                            }}
                        >
                            <span
                                class="drag-handle text-body-secondary"
                                title="Drag to reorder"
                                aria-hidden="true">⠿</span
                            >
                            <input
                                type="checkbox"
                                class="form-check-input mt-0 flex-shrink-0"
                                bind:checked={item.enabled}
                                onclick={(e) => e.stopPropagation()}
                                title="Include in export"
                                aria-label="Include {item.filename}"
                            />
                            <div class="thumb flex-shrink-0">
                                {#if item.preview_path}
                                    <img
                                        src={convertFileSrc(item.preview_path)}
                                        alt=""
                                        draggable="false"
                                    />
                                {:else}
                                    <div class="thumb-missing" title="Preview unavailable">
                                        ⚠
                                    </div>
                                {/if}
                            </div>
                            <div class="flex-grow-1 min-w-0">
                                <div class="text-truncate small fw-medium">
                                    {item.filename}
                                </div>
                                <div class="small d-flex align-items-center gap-1 flex-wrap">
                                    {#if item.enabled}
                                        <code>{numbering.get(
                                                item.source_path,
                                            )}.{item.ext}</code
                                        >
                                        {#if isComplete(item.meta)}
                                            <span class="badge text-bg-success"
                                                >Complete</span
                                            >
                                        {:else}
                                            <span class="badge text-bg-warning"
                                                >Incomplete</span
                                            >
                                        {/if}
                                    {:else}
                                        <span class="text-body-secondary"
                                            >Excluded</span
                                        >
                                    {/if}
                                    {#if item.error}
                                        <span
                                            class="badge text-bg-danger"
                                            title={item.error}>Unreadable</span
                                        >
                                    {/if}
                                </div>
                            </div>
                            <div
                                class="btn-group-vertical flex-shrink-0"
                                role="group"
                            >
                                <button
                                    type="button"
                                    class="btn btn-sm btn-outline-secondary py-0 px-1"
                                    disabled={i === 0}
                                    title="Move up"
                                    aria-label="Move {item.filename} up"
                                    onclick={(e) => {
                                        e.stopPropagation();
                                        move(i, i - 1);
                                    }}>▲</button
                                >
                                <button
                                    type="button"
                                    class="btn btn-sm btn-outline-secondary py-0 px-1"
                                    disabled={i === items.length - 1}
                                    title="Move down"
                                    aria-label="Move {item.filename} down"
                                    onclick={(e) => {
                                        e.stopPropagation();
                                        move(i, i + 1);
                                    }}>▼</button
                                >
                            </div>
                        </div>
                    {/each}
                </div>
            </div>

            <!-- Right: metadata editor -->
            <div class="col-lg-6">
                {#if selected}
                    {@const s = selected}
                    <div class="card editor-card">
                        <div class="card-body">
                            <div class="d-flex gap-3 mb-3">
                                <div class="editor-thumb flex-shrink-0">
                                    {#if s.preview_path}
                                        <img
                                            src={convertFileSrc(s.preview_path)}
                                            alt={s.filename}
                                        />
                                    {:else}
                                        <div class="thumb-missing large">⚠</div>
                                    {/if}
                                </div>
                                <div class="min-w-0">
                                    <div class="fw-semibold text-break">
                                        {s.filename}
                                    </div>
                                    <div class="small text-body-secondary">
                                        {#if s.enabled}
                                            Saves as
                                            <code
                                                >{numbering.get(
                                                    s.source_path,
                                                )}.{s.ext}</code
                                            >
                                        {:else}
                                            Excluded from export
                                        {/if}
                                    </div>
                                    <div class="form-check mt-2">
                                        <input
                                            id="editor-enabled"
                                            type="checkbox"
                                            class="form-check-input"
                                            bind:checked={s.enabled}
                                        />
                                        <label
                                            class="form-check-label small"
                                            for="editor-enabled"
                                            >Include in export</label
                                        >
                                    </div>
                                </div>
                            </div>

                            {#if s.error}
                                <div
                                    class="alert alert-danger small py-2"
                                    role="alert"
                                >
                                    This image couldn't be read
                                    (<code>{s.error}</code>). Exclude it, or the
                                    import may fail.
                                </div>
                            {/if}

                            {#if bulkNote}
                                <div class="small text-success mb-2">
                                    {bulkNote}
                                </div>
                            {/if}

                            <!-- Title (required) -->
                            <div class="mb-3">
                                <label class="form-label mb-1" for="f-title">
                                    Title <span class="text-danger">*</span>
                                </label>
                                <input
                                    id="f-title"
                                    type="text"
                                    class="form-control form-control-sm"
                                    class:is-invalid={s.enabled &&
                                        s.meta.title.trim() === ""}
                                    bind:value={s.meta.title}
                                />
                            </div>

                            <!-- Original date (required) + bulk -->
                            <div class="mb-3">
                                <div
                                    class="d-flex justify-content-between align-items-end"
                                >
                                    <label
                                        class="form-label mb-1"
                                        for="f-original-date"
                                    >
                                        Original date
                                        <span class="text-danger">*</span>
                                    </label>
                                    {@render applyBtn(BULK_FIELDS.original_date)}
                                </div>
                                <input
                                    id="f-original-date"
                                    type="text"
                                    class="form-control form-control-sm"
                                    class:is-invalid={s.enabled &&
                                        s.meta.original_date.trim() === ""}
                                    placeholder="e.g. Summer 1962"
                                    bind:value={s.meta.original_date}
                                />
                            </div>

                            <!-- EDTF date (required) + bulk -->
                            <div class="mb-3">
                                <div
                                    class="d-flex justify-content-between align-items-end"
                                >
                                    <label
                                        class="form-label mb-1"
                                        for="f-edtf"
                                    >
                                        EDTF date
                                        <span class="text-danger">*</span>
                                    </label>
                                    {@render applyBtn(BULK_FIELDS.edtf_date)}
                                </div>
                                <input
                                    id="f-edtf"
                                    type="text"
                                    class="form-control form-control-sm font-monospace"
                                    class:is-invalid={s.enabled &&
                                        s.meta.edtf_date.trim() === ""}
                                    placeholder="e.g. 1962-06 or [1962..1963]"
                                    bind:value={s.meta.edtf_date}
                                />
                                <div class="form-text">
                                    Extended Date/Time Format (validated by the
                                    server on import).
                                </div>
                            </div>

                            <!-- Creator + bulk -->
                            <div class="mb-3">
                                <div
                                    class="d-flex justify-content-between align-items-end"
                                >
                                    <label class="form-label mb-1" for="f-creator"
                                        >Creator</label
                                    >
                                    {@render applyBtn(BULK_FIELDS.creator)}
                                </div>
                                <input
                                    id="f-creator"
                                    type="text"
                                    class="form-control form-control-sm"
                                    bind:value={s.meta.creator}
                                />
                            </div>

                            <!-- Source URL + bulk -->
                            <div class="mb-3">
                                <div
                                    class="d-flex justify-content-between align-items-end"
                                >
                                    <label class="form-label mb-1" for="f-source"
                                        >Source URL</label
                                    >
                                    {@render applyBtn(BULK_FIELDS.source_url)}
                                </div>
                                <input
                                    id="f-source"
                                    type="url"
                                    class="form-control form-control-sm"
                                    placeholder="https://…"
                                    bind:value={s.meta.source_url}
                                />
                            </div>

                            <!-- License + bulk -->
                            <div class="mb-3">
                                <div
                                    class="d-flex justify-content-between align-items-end"
                                >
                                    <label class="form-label mb-1" for="f-license"
                                        >License</label
                                    >
                                    {@render applyBtn(BULK_FIELDS.license_name)}
                                </div>
                                <select
                                    id="f-license"
                                    class="form-select form-select-sm"
                                    bind:value={s.meta.license_name}
                                >
                                    <option value="">— none —</option>
                                    {#each licenses as lic (lic)}
                                        <option value={lic}>{lic}</option>
                                    {/each}
                                </select>
                            </div>

                            <!-- Reference ID (per-image, no bulk) -->
                            <div class="mb-3">
                                <label class="form-label mb-1" for="f-ref"
                                    >Reference ID</label
                                >
                                <input
                                    id="f-ref"
                                    type="text"
                                    class="form-control form-control-sm"
                                    bind:value={s.meta.reference_id}
                                />
                            </div>

                            <!-- Description + bulk -->
                            <div class="mb-3">
                                <div
                                    class="d-flex justify-content-between align-items-end"
                                >
                                    <label class="form-label mb-1" for="f-desc"
                                        >Description</label
                                    >
                                    {@render applyBtn(BULK_FIELDS.description)}
                                </div>
                                <textarea
                                    id="f-desc"
                                    class="form-control form-control-sm"
                                    rows="3"
                                    bind:value={s.meta.description}
                                ></textarea>
                            </div>
                        </div>
                    </div>
                {:else}
                    <p class="text-body-secondary">
                        Select an image to edit its metadata.
                    </p>
                {/if}
            </div>
        </div>
    {/if}
{/if}

{#snippet applyBtn(field: BulkField)}
    <button
        type="button"
        class="btn btn-link btn-sm p-0 lh-1"
        disabled={enabledItems.length < 2}
        title="Apply this value to all other included images"
        onclick={() => applyToAll(field)}
    >
        Apply to all
    </button>
{/snippet}

{#if conflict}
    <div
        class="prep-modal-overlay"
        role="button"
        tabindex="0"
        onclick={() => (conflict = null)}
        onkeydown={(e) => {
            if (e.key === "Escape") conflict = null;
        }}
    >
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="card prep-modal-card shadow" onclick={(e) => e.stopPropagation()}>
            <div class="card-body">
                <h2 class="h5 card-title">
                    Overwrite existing {conflict.field.label}?
                </h2>
                <p class="small text-body-secondary mb-2">
                    {#if conflict.empties.length > 0}
                        {conflict.empties.length} image{conflict.empties
                            .length === 1
                            ? ""
                            : "s"} with no {conflict.field.label} will be filled
                        automatically.
                    {/if}
                    The following already have a different value. Tick the ones
                    to overwrite with
                    <strong>“{displayVal(conflict.value)}”</strong>.
                </p>
                <div class="d-flex gap-3 mb-2 small">
                    <button
                        type="button"
                        class="btn btn-link btn-sm p-0"
                        onclick={() => setAllConflicts(true)}>Select all</button
                    >
                    <button
                        type="button"
                        class="btn btn-link btn-sm p-0"
                        onclick={() => setAllConflicts(false)}
                        >Select none</button
                    >
                </div>
                <div class="prep-conflict-list border rounded">
                    {#each conflict.conflicts as c (c.source_path)}
                        <label
                            class="d-flex align-items-center gap-2 px-2 py-1 border-bottom conflict-item"
                        >
                            <input
                                type="checkbox"
                                class="form-check-input mt-0 flex-shrink-0"
                                bind:checked={conflict.checked[c.source_path]}
                            />
                            <div class="thumb-sm flex-shrink-0">
                                {#if c.preview_path}
                                    <img
                                        src={convertFileSrc(c.preview_path)}
                                        alt=""
                                    />
                                {/if}
                            </div>
                            <span class="flex-grow-1 text-truncate small"
                                >{c.filename}</span
                            >
                            <span
                                class="small text-body-secondary text-truncate conflict-current"
                                >now: {displayVal(
                                    c.meta[conflict.field.key],
                                )}</span
                            >
                        </label>
                    {/each}
                </div>
                <div class="d-flex justify-content-end gap-2 mt-3">
                    <button
                        type="button"
                        class="btn btn-outline-secondary btn-sm"
                        onclick={() => (conflict = null)}>Cancel</button
                    >
                    <button
                        type="button"
                        class="btn btn-primary btn-sm"
                        onclick={confirmBulk}>Apply</button
                    >
                </div>
            </div>
        </div>
    </div>
{/if}

{#if dragPreview}
    <div
        class="drag-ghost"
        use:portal
        style="left: {dragPreview.x}px; top: {dragPreview.y}px;"
    >
        {#if dragPreview.src}
            <img src={dragPreview.src} alt="" />
        {:else}
            <div class="thumb-missing">⚠</div>
        {/if}
    </div>
{/if}

<style>
    .prep-list {
        max-height: calc(100vh - 240px);
        overflow-y: auto;
    }
    .prep-row {
        display: flex;
        align-items: center;
        gap: 0.5rem;
        padding: 0.4rem 0.5rem;
        border: 1px solid var(--bs-border-color);
        border-top-width: 0;
        cursor: pointer;
        background: var(--bs-body-bg);
    }
    .prep-row:first-child {
        border-top-width: 1px;
        border-top-left-radius: var(--bs-border-radius);
        border-top-right-radius: var(--bs-border-radius);
    }
    .prep-row:last-child {
        border-bottom-left-radius: var(--bs-border-radius);
        border-bottom-right-radius: var(--bs-border-radius);
    }
    .prep-row.selected {
        background: var(--bs-primary-bg-subtle);
        border-color: var(--bs-primary);
    }
    .prep-row.excluded {
        opacity: 0.55;
    }
    .prep-row.drag-over {
        border-top: 2px solid var(--bs-primary);
    }
    .drag-handle {
        cursor: grab;
        font-size: 1.1rem;
        line-height: 1;
    }
    .thumb {
        width: 48px;
        height: 48px;
        border-radius: 4px;
        overflow: hidden;
        background: var(--bs-body-bg);
        display: flex;
        align-items: center;
        justify-content: center;
    }
    .drag-ghost {
        position: fixed;
        z-index: 1090;
        width: 44px;
        height: 44px;
        border-radius: 4px;
        overflow: hidden;
        opacity: 0.75;
        pointer-events: none;
        transform: translate(0.75em, 0.75em);
        box-shadow: 0 3px 10px rgba(0, 0, 0, 0.35);
    }
    .drag-ghost img {
        width: 100%;
        height: 100%;
        object-fit: cover;
    }
    .thumb img {
        width: 100%;
        height: 100%;
        object-fit: cover;
    }
    .thumb-missing {
        width: 100%;
        height: 100%;
        display: flex;
        align-items: center;
        justify-content: center;
        color: var(--bs-warning);
        background: var(--bs-warning-bg-subtle);
    }
    .thumb-missing.large {
        width: 140px;
        height: 140px;
        font-size: 2rem;
        border-radius: 6px;
    }
    .min-w-0 {
        min-width: 0;
    }
    .editor-card {
        position: sticky;
        top: 1rem;
    }
    .editor-thumb {
        width: 140px;
        height: 140px;
        border-radius: 6px;
        overflow: hidden;
        background: var(--bs-body-tertiary);
        display: flex;
        align-items: center;
        justify-content: center;
    }
    .editor-thumb img {
        width: 100%;
        height: 100%;
        object-fit: contain;
    }
    .prep-modal-overlay {
        position: fixed;
        inset: 0;
        background: rgba(0, 0, 0, 0.5);
        display: flex;
        align-items: center;
        justify-content: center;
        z-index: 1080;
        padding: 1rem;
        cursor: default;
    }
    .prep-modal-card {
        width: 100%;
        max-width: 520px;
        max-height: 85vh;
        overflow: hidden;
        display: flex;
        cursor: default;
    }
    .prep-conflict-list {
        max-height: 45vh;
        overflow-y: auto;
    }
    .conflict-item {
        cursor: pointer;
        margin: 0;
    }
    .conflict-item:last-child {
        border-bottom: 0 !important;
    }
    .conflict-current {
        max-width: 40%;
    }
    .thumb-sm {
        width: 32px;
        height: 32px;
        border-radius: 3px;
        overflow: hidden;
        background: var(--bs-body-tertiary);
    }
    .thumb-sm img {
        width: 100%;
        height: 100%;
        object-fit: cover;
    }
</style>
