<script lang="ts">
    import { page } from "$app/state";
    import { goto } from "$app/navigation";
    import { auth } from "$lib/auth.svelte";
    import { invoke } from "@tauri-apps/api/core";
    import { listen, type UnlistenFn } from "@tauri-apps/api/event";
    import { open as openDialog } from "@tauri-apps/plugin-dialog";

    type Collection = {
        id: number;
        name: string;
        image_count: number;
        source: { id: number; name: string };
    };

    type Sidecar = {
        filename: string;
        title: string;
        original_date: string;
        edtf_date: string;
        source_url: string | null;
        description: string | null;
        creator: string | null;
        reference_id: string | null;
        license_name: string | null;
        rotation: number;
        mirror: string;
    };

    type PreflightPair = {
        stem: string;
        sidecar_path: string;
        image_path: string;
        sidecar: Sidecar;
    };

    type CompletedPair = {
        stem: string;
        done_path: string;
        image_id: number | null;
    };

    type PreflightError = { source: string; message: string };

    type PreflightResult = {
        folder: string;
        pending: PreflightPair[];
        completed: CompletedPair[];
        errors: PreflightError[];
        license_names: string[];
    };

    type ImportStatus = "pending" | "committed" | "failed";

    type ImportItem = {
        stem: string;
        sidecar_path: string;
        image_path: string;
        status: ImportStatus;
        error?: string;
        image_id?: number;
    };

    type ImportsStateResponse = {
        items: ImportItem[];
        active: boolean;
        paused: boolean;
        folder: string | null;
    };

    type ImportProgress = {
        stem: string;
        status: ImportStatus;
        error?: string;
        image_id?: number;
        done: number;
        total: number;
    };

    type ImportActivity = { stem: string; phase: string };

    type ImportComplete = { paused: boolean; stopped_on_error: boolean };
    type ImportErrorEvent = { message: string };

    let collectionId = $derived(page.params.id!);
    let collection: Collection | null = $state(null);
    let loading = $state(true);
    let loadError = $state("");

    let folder = $state<string | null>(null);
    let preflight = $state<PreflightResult | null>(null);
    let preflightRunning = $state(false);
    let preflightError = $state("");

    let batchItems: ImportItem[] = $state([]);
    let batchActive = $state(false);
    let batchPaused = $state(false);
    let activeStem = $state<string | null>(null);
    let batchError = $state("");
    let starting = $state(false);
    let pausing = $state(false);
    let clearing = $state(false);

    let committedCount = $derived(
        batchItems.filter((i) => i.status === "committed").length,
    );
    let failedCount = $derived(
        batchItems.filter((i) => i.status === "failed").length,
    );
    let pendingCount = $derived(
        batchItems.filter((i) => i.status === "pending").length,
    );
    let totalCount = $derived(batchItems.length);
    let pct = $derived(
        totalCount > 0
            ? Math.round(
                  ((committedCount + failedCount) / totalCount) * 100,
              )
            : 0,
    );
    let canResume = $derived(
        !batchActive && (pendingCount > 0 || failedCount > 0),
    );
    let sortedBatchItems = $derived(
        [...batchItems].sort((a, b) => a.stem.localeCompare(b.stem)),
    );
    let firstFailure = $derived(
        sortedBatchItems.find((i) => i.status === "failed") ?? null,
    );

    function authHeaders(): HeadersInit {
        return auth.accessToken
            ? { Authorization: `Bearer ${auth.accessToken}` }
            : {};
    }

    $effect(() => {
        if (!auth.hydrated) return;
        if (!auth.user) {
            goto("/", { replaceState: true });
            return;
        }
        const id = collectionId;
        void loadCollection(id);
    });

    async function loadCollection(id: string) {
        loading = true;
        loadError = "";
        collection = null;
        try {
            const resp = await fetch(
                `${auth.instanceUrl}/api/v2/collections/${id}/`,
                { headers: authHeaders() },
            );
            if (!resp.ok)
                throw new Error(`Collection: server returned ${resp.status}`);
            collection = await resp.json();
            await refreshBatchState();
        } catch (e) {
            loadError = String(e);
        } finally {
            loading = false;
        }
    }

    async function refreshBatchState() {
        if (!collection) return;
        try {
            const resp = await invoke<ImportsStateResponse>(
                "get_imports_state",
                { collectionId: collection.id },
            );
            batchItems = resp.items;
            batchActive = resp.active;
            batchPaused = resp.paused;
            if (!folder && resp.folder) {
                folder = resp.folder;
            }
        } catch (e) {
            console.error("imports state:", e);
        }
    }

    $effect(() => {
        if (!collection) return;
        const cid = collection.id;
        let cancelled = false;
        const listeners: UnlistenFn[] = [];

        (async () => {
            const register = async (fn: Promise<UnlistenFn>) => {
                const un = await fn;
                if (cancelled) un();
                else listeners.push(un);
            };
            await register(
                listen<ImportProgress>("import-progress", (ev) => {
                    const { stem, status, error, image_id } = ev.payload;
                    batchItems = batchItems.map((i) =>
                        i.stem === stem
                            ? { ...i, status, error, image_id }
                            : i,
                    );
                    if (activeStem === stem) activeStem = null;
                }),
            );
            await register(
                listen<ImportActivity>("import-activity", (ev) => {
                    if (ev.payload.phase === "uploading") {
                        activeStem = ev.payload.stem;
                    }
                }),
            );
            await register(
                listen<ImportComplete>("import-complete", () => {
                    activeStem = null;
                    void refreshBatchState();
                }),
            );
            await register(
                listen<ImportErrorEvent>("import-error", (ev) => {
                    batchError = ev.payload.message;
                }),
            );
            if (!cancelled) void refreshBatchState();
        })();

        return () => {
            cancelled = true;
            listeners.forEach((un) => un());
            if (collection?.id !== cid) {
                batchItems = [];
                activeStem = null;
            }
        };
    });

    async function pickFolder() {
        const selected = await openDialog({
            directory: true,
            multiple: false,
            title: "Choose folder of paired JSON+image files",
        });
        if (typeof selected === "string" && selected !== folder) {
            folder = selected;
            preflight = null;
            preflightError = "";
            await runPreflight();
        }
    }

    async function runPreflight() {
        if (!folder) return;
        preflightRunning = true;
        preflightError = "";
        preflight = null;
        try {
            preflight = await invoke<PreflightResult>("preflight_imports", {
                folder,
                instanceUrl: auth.instanceUrl,
                accessToken: auth.accessToken ?? "",
            });
        } catch (e) {
            preflightError = String(e);
        } finally {
            preflightRunning = false;
        }
    }

    async function startBatch() {
        if (!collection || !preflight || !folder) return;
        if (preflight.errors.length > 0) return;
        if (preflight.pending.length === 0) return;
        if (
            !auth.accessToken ||
            !auth.refreshToken ||
            !auth.instanceUrl
        ) {
            batchError = "Not signed in.";
            return;
        }

        starting = true;
        batchError = "";
        try {
            await invoke("start_imports", {
                collectionId: collection.id,
                folder,
                instanceUrl: auth.instanceUrl,
                accessToken: auth.accessToken,
                refreshToken: auth.refreshToken,
                items: preflight.pending.map((p) => ({
                    stem: p.stem,
                    sidecar_path: p.sidecar_path,
                    image_path: p.image_path,
                })),
            });
            await refreshBatchState();
        } catch (e) {
            batchError = String(e);
        } finally {
            starting = false;
        }
    }

    async function pauseBatch() {
        pausing = true;
        try {
            await invoke("pause_imports");
            await refreshBatchState();
        } catch (e) {
            batchError = String(e);
        } finally {
            pausing = false;
        }
    }

    async function resumeBatch() {
        if (!collection || !folder) return;
        if (
            !auth.accessToken ||
            !auth.refreshToken ||
            !auth.instanceUrl
        ) {
            batchError = "Not signed in.";
            return;
        }
        batchError = "";
        try {
            await invoke("resume_imports", {
                collectionId: collection.id,
                folder,
                instanceUrl: auth.instanceUrl,
                accessToken: auth.accessToken,
                refreshToken: auth.refreshToken,
            });
            await refreshBatchState();
        } catch (e) {
            batchError = String(e);
        }
    }

    async function clearBatch() {
        if (!collection) return;
        clearing = true;
        try {
            await invoke("clear_imports", { collectionId: collection.id });
            await refreshBatchState();
            // Re-running preflight gives us a fresh view of the folder
            // (e.g. after a successful batch finished, any .done files
            // get reflected as completed pairs).
            if (folder) await runPreflight();
        } catch (e) {
            batchError = String(e);
        } finally {
            clearing = false;
        }
    }

    function fileName(path: string): string {
        const parts = path.split(/[\\/]/);
        return parts[parts.length - 1] || path;
    }
</script>

<nav aria-label="breadcrumb">
    <ol class="breadcrumb">
        <li class="breadcrumb-item"><a href="/browse">Sources</a></li>
        {#if collection}
            <li class="breadcrumb-item">
                <a href="/browse/{collection.source.id}"
                    >{collection.source.name}</a
                >
            </li>
            <li class="breadcrumb-item">
                <a href="/collections/{collection.id}">{collection.name}</a>
            </li>
        {/if}
        <li class="breadcrumb-item active" aria-current="page">
            Import images
        </li>
    </ol>
</nav>

{#if loading}
    <div class="d-flex align-items-center text-body-secondary">
        <span
            class="spinner-border spinner-border-sm me-2"
            role="status"
            aria-hidden="true"
        ></span>
        Loading…
    </div>
{:else if loadError}
    <div class="alert alert-danger" role="alert">{loadError}</div>
{:else if collection}
    <h1 class="mb-3">Import images</h1>
    <p class="text-body-secondary mb-4">
        Import a folder of new images into <strong>{collection.name}</strong>.
        Each image must be paired with a JSON sidecar (e.g.
        <code>000001.json</code> + <code>000001.tif</code>) and the numbering must be contiguous.
    </p>

    <!-- Step 1: pick folder + run preflight -->
    <div class="card mb-4">
        <div class="card-body">
            <h2 class="h5 card-title">1. Choose folder</h2>
            <p class="card-text text-body-secondary small mb-3">
                The folder must contain paired sidecar + image files
                named with zero-padded six-digit stems.
            </p>

            <button
                type="button"
                class="btn btn-outline-primary"
                disabled={preflightRunning || batchActive}
                onclick={pickFolder}
            >
                {folder ? "Change folder…" : "Choose folder…"}
            </button>

            {#if folder}
                <div class="small text-body-secondary text-break mt-3 mb-2">
                    <code>{folder}</code>
                </div>
                <button
                    type="button"
                    class="btn btn-link btn-sm p-0"
                    disabled={preflightRunning || batchActive}
                    onclick={runPreflight}
                >
                    {preflightRunning ? "Re-running…" : "Re-run preflight"}
                </button>
            {/if}

            {#if preflightError}
                <div class="alert alert-danger small mt-3 mb-0" role="alert">
                    {preflightError}
                </div>
            {/if}
        </div>
    </div>

    <!-- Step 2: preflight results -->
    {#if preflight}
        <div class="card mb-4">
            <div class="card-body">
                <h2 class="h5 card-title">2. Preflight</h2>
                {#if preflight.errors.length > 0}
                    <div class="alert alert-danger small mb-3" role="alert">
                        <strong
                            >{preflight.errors.length} blocking issue{preflight
                                .errors.length === 1
                                ? ""
                                : "s"}</strong
                        > — fix these before continuing.
                    </div>
                    <div class="table-responsive mb-3">
                        <table class="table table-sm align-middle">
                            <thead>
                                <tr>
                                    <th style="width: 180px;">File</th>
                                    <th>Issue</th>
                                </tr>
                            </thead>
                            <tbody>
                                {#each preflight.errors as err (err.source + err.message)}
                                    <tr>
                                        <td class="text-break"
                                            ><code>{err.source}</code></td
                                        >
                                        <td class="small">{err.message}</td>
                                    </tr>
                                {/each}
                            </tbody>
                        </table>
                    </div>
                {/if}

                <div class="small text-body-secondary mb-3">
                    <strong>{preflight.pending.length.toLocaleString()}</strong>
                    ready ·
                    <strong
                        >{preflight.completed.length.toLocaleString()}</strong
                    >
                    already imported (<code>.json.done</code>)
                </div>

                {#if preflight.completed.length > 0}
                    <details class="mb-3">
                        <summary class="small">
                            Already-imported pairs ({preflight.completed.length})
                        </summary>
                        <ul class="small mt-2 mb-0">
                            {#each preflight.completed as c (c.stem)}
                                <li>
                                    <code>{c.stem}</code>
                                    {#if c.image_id != null}
                                        → image #{c.image_id}
                                    {/if}
                                </li>
                            {/each}
                        </ul>
                    </details>
                {/if}

                {#if preflight.pending.length > 0 && preflight.errors.length === 0 && batchItems.length === 0}
                    <button
                        type="button"
                        class="btn btn-primary"
                        disabled={starting}
                        onclick={startBatch}
                    >
                        {starting
                            ? "Starting…"
                            : `Import ${preflight.pending.length.toLocaleString()} image${preflight.pending.length === 1 ? "" : "s"}`}
                    </button>
                {/if}
            </div>
        </div>
    {/if}

    <!-- Step 3: in-progress / finished batch -->
    {#if batchItems.length > 0}
        <div class="card">
            <div class="card-body">
                <h2 class="h5 card-title">3. Upload</h2>
                <div class="small text-body-secondary mb-1">
                    {#if batchActive && batchPaused}
                        Pausing (letting the current upload finish)…
                    {:else if batchActive}
                        Importing…
                    {:else if pendingCount > 0 || failedCount > 0}
                        Stopped
                    {:else}
                        Finished
                    {/if}
                </div>
                <div class="progress mb-2" role="progressbar">
                    <div
                        class="progress-bar"
                        class:progress-bar-striped={batchActive}
                        class:progress-bar-animated={batchActive &&
                            !batchPaused}
                        style="width: {pct}%"
                    >
                        {pct}%
                    </div>
                </div>
                <div class="small text-body-secondary mb-3">
                    <strong>{committedCount.toLocaleString()}</strong> imported
                    · <strong>{failedCount.toLocaleString()}</strong> failed ·
                    <strong>{pendingCount.toLocaleString()}</strong> pending ·
                    {totalCount.toLocaleString()} total
                </div>

                {#if firstFailure}
                    <div class="alert alert-danger small" role="alert">
                        <strong>Import stopped on
                            <code>{firstFailure.stem}</code>.</strong>
                        {firstFailure.error ?? "(no error message)"}
                        <div class="mt-2">
                            Fix the issue (edit the JSON, replace the image,
                            etc.) and click <strong>Resume</strong> to pick up
                            where it left off.
                        </div>
                    </div>
                {/if}

                {#if batchError}
                    <div class="alert alert-danger small" role="alert">
                        {batchError}
                    </div>
                {/if}

                <div class="d-flex gap-2 mb-3 flex-wrap">
                    {#if batchActive && !batchPaused}
                        <button
                            type="button"
                            class="btn btn-outline-secondary btn-sm"
                            disabled={pausing}
                            onclick={pauseBatch}
                        >
                            {pausing ? "Pausing…" : "Pause"}
                        </button>
                    {/if}
                    {#if canResume}
                        <button
                            type="button"
                            class="btn btn-primary btn-sm"
                            onclick={resumeBatch}
                        >
                            Resume
                        </button>
                    {/if}
                    {#if !batchActive}
                        <button
                            type="button"
                            class="btn btn-outline-danger btn-sm"
                            disabled={clearing}
                            onclick={clearBatch}
                        >
                            {clearing ? "Clearing…" : "Clear batch"}
                        </button>
                    {/if}
                </div>

                <div class="table-responsive">
                    <table class="table table-sm align-middle">
                        <thead>
                            <tr>
                                <th style="width: 90px;">Stem</th>
                                <th>Image file</th>
                                <th style="width: 220px;">Status</th>
                            </tr>
                        </thead>
                        <tbody>
                            {#each sortedBatchItems as item (item.stem)}
                                <tr>
                                    <td><code>{item.stem}</code></td>
                                    <td class="small text-break">
                                        {fileName(item.image_path)}
                                    </td>
                                    <td>
                                        {#if activeStem === item.stem}
                                            <span class="badge text-bg-primary">
                                                <span
                                                    class="spinner-border spinner-border-sm me-1"
                                                    style="width: 0.7em; height: 0.7em;"
                                                    role="status"
                                                    aria-hidden="true"
                                                ></span>
                                                Uploading
                                            </span>
                                        {:else if item.status === "committed"}
                                            <span
                                                class="badge text-bg-success"
                                            >
                                                Imported{#if item.image_id != null}
                                                    {" "}#{item.image_id}
                                                {/if}
                                            </span>
                                        {:else if item.status === "failed"}
                                            <span class="badge text-bg-danger"
                                                >Failed</span
                                            >
                                            {#if item.error}
                                                <div
                                                    class="small text-danger text-break mt-1"
                                                >
                                                    {item.error}
                                                </div>
                                            {/if}
                                        {:else}
                                            <span
                                                class="badge text-bg-secondary"
                                                >Pending</span
                                            >
                                        {/if}
                                    </td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                </div>
            </div>
        </div>
    {/if}
{/if}
