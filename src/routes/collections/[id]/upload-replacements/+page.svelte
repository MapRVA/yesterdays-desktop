<script lang="ts">
    import { page } from "$app/state";
    import { goto } from "$app/navigation";
    import { auth } from "$lib/auth.svelte";
    import { invoke, convertFileSrc } from "@tauri-apps/api/core";
    import { listen, type UnlistenFn } from "@tauri-apps/api/event";
    import { open as openDialog } from "@tauri-apps/plugin-dialog";
    import { SvelteSet } from "svelte/reactivity";

    type Collection = {
        id: number;
        name: string;
        image_count: number;
        source: { id: number; name: string };
    };

    type ThumbnailItem = { id: number; url: string };

    type ThumbnailPhase = "downloading" | "hashing";

    type ThumbnailProgress = {
        phase: ThumbnailPhase;
        done: number;
        total: number;
        ok: number;
        skipped: number;
        failed: number;
        hashed: number;
        last_id: number;
    };

    type ThumbnailResult = {
        cache_dir: string;
        total: number;
        ok: number;
        skipped: number;
        failed: number;
        hashed: number;
        already_hashed: number;
    };

    type CacheStatus = {
        cache_dir: string;
        exists: boolean;
        thumbnail_count: number;
        hashed_count: number;
    };

    let collectionId = $derived(page.params.id!);
    let collection: Collection | null = $state(null);
    let loading = $state(true);
    let error = $state("");

    let items: ThumbnailItem[] = $state([]);
    let collecting = $state(false);

    let cacheStatus = $state<CacheStatus | null>(null);
    let clearing = $state(false);

    let downloadState = $state<"idle" | "running" | "done" | "error">("idle");
    let progress = $state<ThumbnailProgress | null>(null);
    let result: ThumbnailResult | null = $state(null);
    let downloadError = $state("");
    type HashErrorEvent = { image_id: number; path: string; error: string };
    let hashErrors: HashErrorEvent[] = $state([]);

    let replacementDir: string | null = $state(null);

    type MatchPhase = "hashing_thumbnails" | "matching";
    type MatchProgress = { phase: MatchPhase; done: number; total: number };
    type FolderHashProgress = { done: number; total: number };
    type FolderHashResult = {
        folder: string;
        total: number;
        hashed: number;
        cached: number;
        failed: number;
    };
    type Match = {
        image_id: number;
        thumbnail_path: string;
        file_path: string;
        file_preview_path: string;
        distance: number;
    };
    type MatchResult = {
        matches: Match[];
        unmatched_thumbnails: number[];
        unmatched_files: string[];
        threshold: number;
    };

    const DEFAULT_DISTANCE = 15;

    let matchState = $state<"idle" | "running" | "done" | "error">("idle");
    let matchProgress = $state<MatchProgress | null>(null);
    let matchResult = $state<MatchResult | null>(null);
    let matchError = $state("");

    let folderHashState = $state<"idle" | "running" | "done" | "error">("idle");
    let folderHashProgress = $state<FolderHashProgress | null>(null);
    let folderHashResult = $state<FolderHashResult | null>(null);
    let folderHashError = $state("");
    let folderHashPct = $derived(
        folderHashProgress && folderHashProgress.total > 0
            ? Math.round(
                  (folderHashProgress.done / folderHashProgress.total) * 100,
              )
            : 0,
    );

    // --- Replacements (upload) state -----------------------------------
    type ReplacementStatus = "pending" | "committed" | "failed";
    type ReplacementItem = {
        image_id: number;
        file_path: string;
        status: ReplacementStatus;
        error?: string;
    };
    type ReplacementsStateResponse = {
        items: ReplacementItem[];
        active: boolean;
        paused: boolean;
    };
    type ReplacementProgress = {
        image_id: number;
        status: ReplacementStatus;
        error?: string;
        done: number;
        total: number;
    };
    type ReplacementActivity = { image_id: number; phase: string };

    let replacementsItems: ReplacementItem[] = $state([]);
    let replacementsActive = $state(false);
    let replacementsPaused = $state(false);
    let activeUploads = new SvelteSet<number>();
    let replacementsError = $state("");
    let startingReplacements = $state(false);
    let pausingReplacements = $state(false);
    let clearingReplacements = $state(false);
    let deleteOnSuccess = $state(false);

    let replCommitted = $derived(
        replacementsItems.filter((i) => i.status === "committed").length,
    );
    let replFailed = $derived(
        replacementsItems.filter((i) => i.status === "failed").length,
    );
    let replPending = $derived(
        replacementsItems.filter((i) => i.status === "pending").length,
    );
    let replTotal = $derived(replacementsItems.length);
    let replPct = $derived(
        replTotal > 0
            ? Math.round(((replCommitted + replFailed) / replTotal) * 100)
            : 0,
    );
    let canResume = $derived(
        !replacementsActive && (replPending > 0 || replFailed > 0),
    );
    let sortedReplacementsItems = $derived(
        [...replacementsItems].sort((a, b) => a.image_id - b.image_id),
    );

    let canMatch = $derived(
        (cacheStatus?.thumbnail_count ?? 0) > 0 &&
            !!replacementDir &&
            folderHashState === "done" &&
            matchState !== "running" &&
            downloadState !== "running",
    );

    const PHASE_LABELS: Record<MatchPhase, string> = {
        hashing_thumbnails: "Hashing thumbnails",
        matching: "Matching",
    };

    let matchPct = $derived(
        matchProgress && matchProgress.total > 0
            ? Math.round((matchProgress.done / matchProgress.total) * 100)
            : 0,
    );

    function fileName(path: string): string {
        const parts = path.split(/[\\/]/);
        return parts[parts.length - 1] || path;
    }

    // --- Review state ---------------------------------------------------
    let reviewIndex = $state(0);
    let reviewDecisions: Record<number, boolean> = $state({});
    let lastActedId: number | null = $state(null);

    let sortedMatches = $derived(
        matchResult
            ? [...matchResult.matches].sort(
                  (a, b) => b.distance - a.distance,
              )
            : [],
    );
    let matchesById = $derived(
        matchResult
            ? [...matchResult.matches].sort(
                  (a, b) => a.image_id - b.image_id,
              )
            : [],
    );
    let reviewTotal = $derived(sortedMatches.length);
    let currentMatch = $derived(sortedMatches[reviewIndex] ?? null);
    let decidedCount = $derived(Object.keys(reviewDecisions).length);
    let reviewComplete = $derived(
        reviewTotal > 0 && decidedCount >= reviewTotal,
    );
    let acceptedCount = $derived(
        Object.values(reviewDecisions).filter(Boolean).length,
    );

    function resetReview() {
        reviewIndex = 0;
        reviewDecisions = {};
        lastActedId = null;
    }

    function decide(accepted: boolean) {
        if (!currentMatch) return;
        reviewDecisions = {
            ...reviewDecisions,
            [currentMatch.image_id]: accepted,
        };
        lastActedId = currentMatch.image_id;
        reviewIndex = Math.min(reviewIndex + 1, reviewTotal);
    }

    function undo() {
        if (lastActedId === null) return;
        const next = { ...reviewDecisions };
        delete next[lastActedId];
        reviewDecisions = next;
        reviewIndex = Math.max(0, reviewIndex - 1);
        lastActedId = null;
    }

    function flipDecision(imageId: number) {
        reviewDecisions = {
            ...reviewDecisions,
            [imageId]: !reviewDecisions[imageId],
        };
    }

    $effect(() => {
        if (matchState !== "done" || reviewComplete) return;
        const handler = (e: KeyboardEvent) => {
            // Ignore when typing into inputs
            const tag = (e.target as HTMLElement | null)?.tagName;
            if (tag === "INPUT" || tag === "TEXTAREA") return;

            if (e.key === "ArrowRight" || e.key === "y" || e.key === "Y") {
                e.preventDefault();
                decide(true);
            } else if (
                e.key === "ArrowLeft" ||
                e.key === "n" ||
                e.key === "N"
            ) {
                e.preventDefault();
                decide(false);
            } else if (e.key === "u" || e.key === "U" || e.key === "z" || e.key === "Z") {
                e.preventDefault();
                undo();
            }
        };
        window.addEventListener("keydown", handler);
        return () => window.removeEventListener("keydown", handler);
    });

    $effect(() => {
        if (!auth.hydrated) return;
        if (!auth.user) {
            goto("/", { replaceState: true });
            return;
        }
        const id = collectionId;
        void load(id);
    });

    async function load(id: string) {
        loading = true;
        error = "";
        collection = null;
        items = [];
        try {
            const headers: HeadersInit = auth.accessToken
                ? { Authorization: `Bearer ${auth.accessToken}` }
                : {};
            const resp = await fetch(
                `${auth.instanceUrl}/api/v2/collections/${id}/`,
                { headers },
            );
            if (!resp.ok)
                throw new Error(`Collection: server returned ${resp.status}`);
            collection = await resp.json();
            void refreshCacheStatus();
            void collectThumbnails(id);
        } catch (e) {
            error = String(e);
        } finally {
            loading = false;
        }
    }

    async function collectThumbnails(id: string) {
        collecting = true;
        try {
            const headers: HeadersInit = auth.accessToken
                ? { Authorization: `Bearer ${auth.accessToken}` }
                : {};
            const collected: ThumbnailItem[] = [];
            let url: string | null =
                `${auth.instanceUrl}/api/v2/images/?collection=${id}&page_size=100`;
            while (url) {
                const resp = await fetch(url, { headers });
                if (!resp.ok)
                    throw new Error(`Images: server returned ${resp.status}`);
                const data = await resp.json();
                for (const img of data.results) {
                    if (img.thumbnail) {
                        collected.push({ id: img.id, url: img.thumbnail });
                    }
                }
                url = data.next;
            }
            items = collected;
        } catch (e) {
            error = String(e);
        } finally {
            collecting = false;
        }
    }

    async function startDownload() {
        if (!collection) return;
        downloadState = "running";
        downloadError = "";
        hashErrors = [];
        progress = {
            phase: "downloading",
            done: 0,
            total: items.length,
            ok: 0,
            skipped: 0,
            failed: 0,
            hashed: 0,
            last_id: 0,
        };
        const unlisteners: UnlistenFn[] = [];
        try {
            unlisteners.push(
                await listen<ThumbnailProgress>(
                    "thumbnail-progress",
                    (ev) => {
                        progress = ev.payload;
                    },
                ),
            );
            unlisteners.push(
                await listen<HashErrorEvent>(
                    "thumbnail-hash-error",
                    (ev) => {
                        hashErrors = [...hashErrors, ev.payload];
                    },
                ),
            );
            result = await invoke<ThumbnailResult>("download_thumbnails", {
                collectionId: collection.id,
                items,
            });
            downloadState = "done";
            void refreshCacheStatus();
        } catch (e) {
            downloadError = String(e);
            downloadState = "error";
        } finally {
            unlisteners.forEach((un) => un());
        }
    }

    async function refreshCacheStatus() {
        if (!collection) return;
        try {
            cacheStatus = await invoke<CacheStatus>(
                "get_thumbnail_cache_status",
                { collectionId: collection.id },
            );
        } catch (e) {
            // Non-fatal; cache status just stays stale.
            console.error("cache status:", e);
        }
    }

    async function clearCache() {
        if (!collection) return;
        clearing = true;
        try {
            await invoke("clear_thumbnail_cache", {
                collectionId: collection.id,
            });
            downloadState = "idle";
            progress = null;
            result = null;
            downloadError = "";
            hashErrors = [];
            matchState = "idle";
            matchProgress = null;
            matchResult = null;
            matchError = "";
            resetReview();
            await refreshCacheStatus();
        } catch (e) {
            error = String(e);
        } finally {
            clearing = false;
        }
    }

    async function pickFolder() {
        const selected = await openDialog({
            directory: true,
            multiple: false,
            title: "Choose folder with replacement files",
        });
        if (typeof selected === "string" && selected !== replacementDir) {
            replacementDir = selected;
            folderHashState = "idle";
            folderHashProgress = null;
            folderHashResult = null;
            folderHashError = "";
            matchState = "idle";
            matchProgress = null;
            matchResult = null;
            matchError = "";
            resetReview();
        }
    }

    async function hashFolder() {
        if (!collection || !replacementDir) return;
        folderHashState = "running";
        folderHashError = "";
        folderHashResult = null;
        folderHashProgress = { done: 0, total: 0 };
        // A re-hash invalidates any prior match against this folder.
        matchState = "idle";
        matchProgress = null;
        matchResult = null;
        matchError = "";
        resetReview();
        let unlisten: UnlistenFn | null = null;
        try {
            unlisten = await listen<FolderHashProgress>(
                "folder-hash-progress",
                (ev) => {
                    folderHashProgress = ev.payload;
                },
            );
            folderHashResult = await invoke<FolderHashResult>(
                "hash_replacement_folder",
                { collectionId: collection.id, folder: replacementDir },
            );
            folderHashState = "done";
        } catch (e) {
            folderHashError = String(e);
            folderHashState = "error";
        } finally {
            if (unlisten) unlisten();
        }
    }

    async function runMatch() {
        if (!collection || !replacementDir) return;
        matchState = "running";
        matchError = "";
        matchResult = null;
        matchProgress = null;
        hashErrors = [];
        resetReview();
        const unlisteners: UnlistenFn[] = [];
        try {
            unlisteners.push(
                await listen<MatchProgress>("match-progress", (ev) => {
                    matchProgress = ev.payload;
                }),
            );
            unlisteners.push(
                await listen<HashErrorEvent>(
                    "thumbnail-hash-error",
                    (ev) => {
                        hashErrors = [...hashErrors, ev.payload];
                    },
                ),
            );
            matchResult = await invoke<MatchResult>("match_replacement_folder", {
                collectionId: collection.id,
                folder: replacementDir,
                maxDistance: DEFAULT_DISTANCE,
            });
            matchState = "done";
            void refreshCacheStatus();
        } catch (e) {
            matchError = String(e);
            matchState = "error";
        } finally {
            unlisteners.forEach((un) => un());
        }
    }

    let progressPct = $derived(
        progress && progress.total > 0
            ? Math.round((progress.done / progress.total) * 100)
            : 0,
    );

    // --- Replacements: load, subscribe, actions ------------------------
    async function refreshReplacementsState() {
        if (!collection) return;
        try {
            const resp = await invoke<ReplacementsStateResponse>(
                "get_replacements_state",
                { collectionId: collection.id },
            );
            replacementsItems = resp.items;
            replacementsActive = resp.active;
            replacementsPaused = resp.paused;
        } catch (e) {
            console.error("replacements state:", e);
        }
    }

    $effect(() => {
        if (!collection) return;
        const collectionId = collection.id;
        let cancelled = false;
        const listeners: UnlistenFn[] = [];

        (async () => {
            const register = async (fn: Promise<UnlistenFn>) => {
                const un = await fn;
                if (cancelled) {
                    un();
                } else {
                    listeners.push(un);
                }
            };
            await register(
                listen<ReplacementProgress>(
                    "replacement-progress",
                    (ev) => {
                        const { image_id, status, error } = ev.payload;
                        replacementsItems = replacementsItems.map((i) =>
                            i.image_id === image_id
                                ? { ...i, status, error }
                                : i,
                        );
                        activeUploads.delete(image_id);
                    },
                ),
            );
            await register(
                listen<ReplacementActivity>(
                    "replacement-activity",
                    (ev) => {
                        if (ev.payload.phase === "uploading") {
                            activeUploads.add(ev.payload.image_id);
                        }
                    },
                ),
            );
            await register(
                listen<{ paused: boolean }>(
                    "replacement-complete",
                    () => {
                        activeUploads.clear();
                        void refreshReplacementsState();
                    },
                ),
            );
            await register(
                listen<{ message: string }>(
                    "replacement-error",
                    (ev) => {
                        replacementsError = ev.payload.message;
                    },
                ),
            );
            if (!cancelled) void refreshReplacementsState();
        })();

        return () => {
            cancelled = true;
            listeners.forEach((un) => un());
            // Guard against events arriving after navigation to another collection.
            if (collection?.id !== collectionId) {
                replacementsItems = [];
                activeUploads.clear();
            }
        };
    });

    async function startReplacements() {
        if (!collection || !matchResult) return;
        if (
            !auth.accessToken ||
            !auth.refreshToken ||
            !auth.instanceUrl
        ) {
            replacementsError = "Not signed in.";
            return;
        }
        const accepted = sortedMatches.filter(
            (m) => reviewDecisions[m.image_id],
        );
        if (accepted.length === 0) return;

        startingReplacements = true;
        replacementsError = "";
        try {
            await invoke("start_replacements", {
                collectionId: collection.id,
                instanceUrl: auth.instanceUrl,
                accessToken: auth.accessToken,
                refreshToken: auth.refreshToken,
                items: accepted.map((m) => ({
                    image_id: m.image_id,
                    file_path: m.file_path,
                })),
                deleteOnSuccess,
            });
            await refreshReplacementsState();
        } catch (e) {
            replacementsError = String(e);
        } finally {
            startingReplacements = false;
        }
    }

    async function pauseReplacementsAction() {
        pausingReplacements = true;
        try {
            await invoke("pause_replacements");
            await refreshReplacementsState();
        } catch (e) {
            replacementsError = String(e);
        } finally {
            pausingReplacements = false;
        }
    }

    async function resumeReplacementsAction() {
        if (!collection) return;
        if (
            !auth.accessToken ||
            !auth.refreshToken ||
            !auth.instanceUrl
        ) {
            replacementsError = "Not signed in.";
            return;
        }
        replacementsError = "";
        try {
            await invoke("resume_replacements", {
                collectionId: collection.id,
                instanceUrl: auth.instanceUrl,
                accessToken: auth.accessToken,
                refreshToken: auth.refreshToken,
            });
            await refreshReplacementsState();
        } catch (e) {
            replacementsError = String(e);
        }
    }

    async function clearReplacementsAction() {
        if (!collection) return;
        clearingReplacements = true;
        try {
            await invoke("clear_replacements", {
                collectionId: collection.id,
            });
            await refreshReplacementsState();
        } catch (e) {
            replacementsError = String(e);
        } finally {
            clearingReplacements = false;
        }
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
            Upload higher-res replacements
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
{:else if error}
    <div class="alert alert-danger" role="alert">{error}</div>
{:else if collection}
    <h1 class="mb-3">Upload higher-res replacements</h1>
    <p class="text-body-secondary mb-4">
        Replace the images in <strong>{collection.name}</strong> with higher-resolution
        versions from a local folder.
    </p>

    <div class="row g-4">
        <!-- Step 1: download thumbnails -->
        <div class="col-md-6">
            <div class="card h-100">
                <div class="card-body">
                    <h2 class="h5 card-title">1. Reload thumbnails</h2>
                    <p class="card-text text-body-secondary small mb-3">
                        Downloads (or re-uses cached) thumbnails and hashes
                        them, in one pass.
                    </p>

                    {#if cacheStatus}
                        <div
                            class="d-flex align-items-center justify-content-between small mb-3 p-2 rounded bg-body-tertiary"
                        >
                            <div class="text-body-secondary">
                                {#if cacheStatus.exists && cacheStatus.thumbnail_count > 0}
                                    <strong
                                        >{cacheStatus.thumbnail_count.toLocaleString()}</strong
                                    >
                                    cached ·
                                    <strong
                                        >{cacheStatus.hashed_count.toLocaleString()}</strong
                                    >
                                    hashed
                                {:else}
                                    No cache yet
                                {/if}
                            </div>
                            {#if cacheStatus.exists && cacheStatus.thumbnail_count > 0}
                                <button
                                    type="button"
                                    class="btn btn-link btn-sm p-0 text-danger"
                                    disabled={clearing ||
                                        downloadState === "running" ||
                                        matchState === "running"}
                                    onclick={clearCache}
                                >
                                    {clearing ? "Clearing…" : "Clear cache"}
                                </button>
                            {/if}
                        </div>
                    {/if}

                    {#if collecting}
                        <div class="text-body-secondary small d-flex align-items-center">
                            <span
                                class="spinner-border spinner-border-sm me-2"
                                role="status"
                                aria-hidden="true"
                            ></span>
                            Preparing image list…
                        </div>
                    {:else if downloadState === "idle"}
                        <button
                            type="button"
                            class="btn btn-primary"
                            disabled={items.length === 0}
                            onclick={startDownload}
                        >
                            Reload {items.length.toLocaleString()} thumbnails
                        </button>
                        {#if items.length === 0}
                            <p class="text-body-secondary small mt-2 mb-0">
                                No thumbnails available for this collection.
                            </p>
                        {/if}
                    {:else if downloadState === "running" && progress}
                        <div class="small text-body-secondary mb-1">
                            {progress.phase === "downloading"
                                ? "Downloading"
                                : "Hashing"}
                        </div>
                        <div class="progress mb-2" role="progressbar">
                            <div
                                class="progress-bar progress-bar-striped progress-bar-animated"
                                style="width: {progressPct}%"
                            >
                                {progressPct}%
                            </div>
                        </div>
                        <div class="small text-body-secondary">
                            {progress.done.toLocaleString()} of
                            {progress.total.toLocaleString()} ·
                            {progress.ok} downloaded ·
                            {progress.skipped} cached ·
                            {progress.failed} failed ·
                            {progress.hashed} hashed
                        </div>
                    {:else if downloadState === "done" && result}
                        <div class="alert alert-success small mb-2" role="alert">
                            Finished: {result.ok} downloaded,
                            {result.skipped} already cached,
                            {result.failed} failed.
                            {#if result.already_hashed > 0}
                                {result.hashed} newly hashed ·
                                {result.already_hashed} kept from a previous run.
                            {:else}
                                {result.hashed} hashed.
                            {/if}
                        </div>
                        <div class="small text-body-secondary text-break">
                            Cached at <code>{result.cache_dir}</code>
                        </div>
                        {#if hashErrors.length > 0}
                            <details class="mt-2">
                                <summary class="small text-danger">
                                    {hashErrors.length} hash error{hashErrors.length === 1 ? "" : "s"}
                                </summary>
                                <ul class="small mt-2 mb-0">
                                    {#each hashErrors as err}
                                        <li class="text-break">
                                            #{err.image_id}: {err.error}
                                        </li>
                                    {/each}
                                </ul>
                            </details>
                        {/if}
                    {:else if downloadState === "error"}
                        <div class="alert alert-danger small" role="alert">
                            {downloadError}
                        </div>
                        <button
                            type="button"
                            class="btn btn-outline-primary btn-sm"
                            onclick={startDownload}
                        >
                            Retry
                        </button>
                    {/if}
                </div>
            </div>
        </div>

        <!-- Step 2: pick folder + hash it -->
        <div class="col-md-6">
            <div class="card h-100">
                <div class="card-body">
                    <h2 class="h5 card-title">2. Choose and hash folder</h2>
                    <p class="card-text text-body-secondary small mb-3">
                        Folder containing the higher-res files. Hashing also
                        generates the preview images shown during review.
                    </p>

                    <button
                        type="button"
                        class="btn btn-outline-primary"
                        disabled={folderHashState === "running"}
                        onclick={pickFolder}
                    >
                        {replacementDir ? "Change folder…" : "Choose folder…"}
                    </button>

                    {#if replacementDir}
                        <div class="small text-body-secondary text-break mt-3 mb-3">
                            <code>{replacementDir}</code>
                        </div>

                        {#if folderHashState === "idle"}
                            <button
                                type="button"
                                class="btn btn-primary"
                                onclick={hashFolder}
                            >
                                Hash folder
                            </button>
                        {:else if folderHashState === "running" && folderHashProgress}
                            <div class="small text-body-secondary mb-1">
                                Hashing folder
                            </div>
                            <div class="progress mb-2" role="progressbar">
                                <div
                                    class="progress-bar progress-bar-striped progress-bar-animated"
                                    style="width: {folderHashPct}%"
                                >
                                    {folderHashPct}%
                                </div>
                            </div>
                            <div class="small text-body-secondary">
                                {folderHashProgress.done.toLocaleString()} of
                                {folderHashProgress.total.toLocaleString()}
                            </div>
                        {:else if folderHashState === "done" && folderHashResult}
                            <div class="alert alert-success small mb-2" role="alert">
                                {folderHashResult.total.toLocaleString()} file{folderHashResult.total === 1 ? "" : "s"}:
                                {folderHashResult.hashed} newly hashed,
                                {folderHashResult.cached} from cache,
                                {folderHashResult.failed} failed.
                            </div>
                            <button
                                type="button"
                                class="btn btn-outline-secondary btn-sm"
                                onclick={hashFolder}
                            >
                                Re-hash folder
                            </button>
                        {:else if folderHashState === "error"}
                            <div class="alert alert-danger small" role="alert">
                                {folderHashError}
                            </div>
                            <button
                                type="button"
                                class="btn btn-outline-primary btn-sm"
                                onclick={hashFolder}
                            >
                                Retry
                            </button>
                        {/if}
                    {/if}
                </div>
            </div>
        </div>
    </div>

    <!-- Step 3: match -->
    <div class="mt-4">
        <div class="card">
            <div class="card-body">
                <h2 class="h5 card-title">3. Match files to images</h2>
                <p class="card-text text-body-secondary small mb-3">
                    Compares perceptual hashes of each replacement file against
                    the downloaded thumbnails to find one-to-one pairs.
                </p>

                {#if matchState === "idle"}
                    <button
                        type="button"
                        class="btn btn-primary"
                        disabled={!canMatch}
                        onclick={runMatch}
                    >
                        Run match
                    </button>
                    {#if !canMatch}
                        <p class="text-body-secondary small mt-2 mb-0">
                            Finish steps 1 and 2 first (including hashing the
                            folder).
                        </p>
                    {/if}
                {:else if matchState === "running" && matchProgress}
                    <div class="small text-body-secondary mb-1">
                        {PHASE_LABELS[matchProgress.phase]}
                    </div>
                    <div class="progress mb-2" role="progressbar">
                        <div
                            class="progress-bar progress-bar-striped progress-bar-animated"
                            style="width: {matchPct}%"
                        >
                            {matchPct}%
                        </div>
                    </div>
                    <div class="small text-body-secondary">
                        {matchProgress.done.toLocaleString()} of
                        {matchProgress.total.toLocaleString()}
                    </div>
                {:else if matchState === "error"}
                    <div class="alert alert-danger small" role="alert">
                        {matchError}
                    </div>
                    <button
                        type="button"
                        class="btn btn-outline-primary btn-sm"
                        onclick={runMatch}
                    >
                        Retry
                    </button>
                {:else if matchState === "done" && matchResult}
                    <div class="alert alert-success small mb-3" role="alert">
                        <strong>{matchResult.matches.length}</strong> matched ·
                        <strong>{matchResult.unmatched_files.length}</strong>
                        file{matchResult.unmatched_files.length === 1 ? "" : "s"}
                        unmatched ·
                        <strong>{matchResult.unmatched_thumbnails.length}</strong>
                        thumbnail{matchResult.unmatched_thumbnails.length === 1 ? "" : "s"}
                        unmatched (threshold {matchResult.threshold})
                    </div>
                    {#if hashErrors.length > 0}
                        <details class="mb-3">
                            <summary class="small text-danger">
                                {hashErrors.length} thumbnail hash error{hashErrors.length === 1 ? "" : "s"}
                            </summary>
                            <ul class="small mt-2 mb-0">
                                {#each hashErrors as err}
                                    <li class="text-break">
                                        #{err.image_id}: {err.error}
                                    </li>
                                {/each}
                            </ul>
                        </details>
                    {/if}

                    {#if sortedMatches.length > 0 && !reviewComplete && currentMatch}
                        <div class="mb-2 d-flex align-items-center justify-content-between">
                            <div class="small text-body-secondary">
                                Review {reviewIndex + 1} of {reviewTotal}
                                <span class="ms-2 badge text-bg-secondary">
                                    distance {currentMatch.distance}
                                </span>
                            </div>
                            <div class="small text-body-secondary">
                                Sorted worst-first · use
                                <kbd>Y</kbd>/<kbd>→</kbd> accept,
                                <kbd>N</kbd>/<kbd>←</kbd> reject,
                                <kbd>U</kbd> undo
                            </div>
                        </div>
                        <div class="progress mb-3" style="height: 4px;">
                            <div
                                class="progress-bar"
                                style="width: {(decidedCount / reviewTotal) *
                                    100}%"
                            ></div>
                        </div>
                        <div class="row g-3 align-items-stretch mb-3">
                            <div class="col-6 text-center">
                                <div class="small text-body-secondary mb-1">
                                    Cached thumbnail (Image #{currentMatch.image_id})
                                </div>
                                <div
                                    class="d-flex align-items-center justify-content-center bg-body-tertiary rounded"
                                    style="height: 400px; overflow: hidden;"
                                >
                                    <img
                                        src={convertFileSrc(
                                            currentMatch.thumbnail_path,
                                        )}
                                        alt="Image #{currentMatch.image_id}"
                                        style="max-width: 100%; max-height: 100%; object-fit: contain;"
                                    />
                                </div>
                            </div>
                            <div class="col-6 text-center">
                                <div class="small text-body-secondary mb-1 text-break">
                                    {fileName(currentMatch.file_path)}
                                </div>
                                <div
                                    class="d-flex align-items-center justify-content-center bg-body-tertiary rounded"
                                    style="height: 400px; overflow: hidden;"
                                >
                                    <img
                                        src={convertFileSrc(
                                            currentMatch.file_preview_path,
                                        )}
                                        alt={fileName(currentMatch.file_path)}
                                        style="max-width: 100%; max-height: 100%; object-fit: contain;"
                                    />
                                </div>
                            </div>
                        </div>
                        <div class="d-flex justify-content-center gap-2">
                            <button
                                type="button"
                                class="btn btn-outline-danger"
                                onclick={() => decide(false)}
                            >
                                Reject (N)
                            </button>
                            <button
                                type="button"
                                class="btn btn-outline-secondary"
                                disabled={lastActedId === null}
                                onclick={undo}
                            >
                                Undo (U)
                            </button>
                            <button
                                type="button"
                                class="btn btn-success"
                                onclick={() => decide(true)}
                            >
                                Accept (Y)
                            </button>
                        </div>
                    {:else if sortedMatches.length > 0 && reviewComplete}
                        <div class="alert alert-info small mb-3" role="alert">
                            Review complete:
                            <strong>{acceptedCount}</strong> accepted,
                            <strong>{reviewTotal - acceptedCount}</strong>
                            rejected. Click a row to flip a decision.
                        </div>
                        <div class="table-responsive mb-3">
                            <table class="table align-middle">
                                <thead>
                                    <tr>
                                        <th></th>
                                        <th>Cached</th>
                                        <th>Replacement</th>
                                        <th>Image ID</th>
                                        <th>Replacement file</th>
                                        <th class="text-end">Distance</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {#each matchesById as m (m.image_id)}
                                        <tr
                                            role="button"
                                            onclick={() =>
                                                flipDecision(m.image_id)}
                                            class:table-success={reviewDecisions[
                                                m.image_id
                                            ]}
                                            class:table-secondary={!reviewDecisions[
                                                m.image_id
                                            ]}
                                        >
                                            <td style="width: 40px;">
                                                {#if reviewDecisions[m.image_id]}
                                                    ✓
                                                {:else}
                                                    ✗
                                                {/if}
                                            </td>
                                            <td style="width: 80px;">
                                                <img
                                                    src={convertFileSrc(
                                                        m.thumbnail_path,
                                                    )}
                                                    alt="Image #{m.image_id}"
                                                    style="width: 64px; height: 64px; object-fit: cover;"
                                                    loading="lazy"
                                                />
                                            </td>
                                            <td style="width: 80px;">
                                                <img
                                                    src={convertFileSrc(
                                                        m.file_preview_path,
                                                    )}
                                                    alt={fileName(m.file_path)}
                                                    style="width: 64px; height: 64px; object-fit: cover;"
                                                    loading="lazy"
                                                />
                                            </td>
                                            <td>#{m.image_id}</td>
                                            <td class="small text-break">
                                                {fileName(m.file_path)}
                                            </td>
                                            <td class="text-end">
                                                {m.distance}
                                            </td>
                                        </tr>
                                    {/each}
                                </tbody>
                            </table>
                        </div>
                    {/if}

                    {#if matchResult.unmatched_files.length > 0}
                        <details class="mt-4 mb-3">
                            <summary class="small">
                                Unmatched files ({matchResult.unmatched_files.length})
                            </summary>
                            <ul class="small mt-2">
                                {#each matchResult.unmatched_files as f}
                                    <li class="text-break">{fileName(f)}</li>
                                {/each}
                            </ul>
                        </details>
                    {/if}

                    {#if matchResult.unmatched_thumbnails.length > 0}
                        <details>
                            <summary class="small">
                                Unmatched thumbnails ({matchResult.unmatched_thumbnails.length})
                            </summary>
                            <ul class="small mt-2">
                                {#each [...matchResult.unmatched_thumbnails].sort((a, b) => a - b) as id}
                                    <li>#{id}</li>
                                {/each}
                            </ul>
                        </details>
                    {/if}
                {/if}
            </div>
        </div>
    </div>

    {#if replacementsItems.length > 0 || (matchState === "done" && reviewComplete && acceptedCount > 0)}
        <div class="mt-4">
            <div class="card">
                <div class="card-body">
                    <h2 class="h5 card-title">4. Upload replacements</h2>
                    <div class="alert alert-warning small mb-3" role="alert">
                        <strong>Files are uploaded as-is.</strong>
                        No rotation or mirror is applied during upload — the
                        file you choose becomes the canonical image. Make sure
                        each file is correctly oriented before continuing.
                    </div>

                    {#if replacementsItems.length === 0}
                        <div class="form-check mb-3">
                            <input
                                class="form-check-input"
                                type="checkbox"
                                id="delete-on-success"
                                bind:checked={deleteOnSuccess}
                            />
                            <label
                                class="form-check-label small"
                                for="delete-on-success"
                            >
                                Delete each local file after it uploads
                                successfully
                            </label>
                        </div>
                        <button
                            type="button"
                            class="btn btn-primary"
                            disabled={startingReplacements ||
                                acceptedCount === 0}
                            onclick={startReplacements}
                        >
                            {startingReplacements
                                ? "Starting…"
                                : `Upload ${acceptedCount.toLocaleString()} replacement${acceptedCount === 1 ? "" : "s"}`}
                        </button>
                    {:else}
                        <div class="small text-body-secondary mb-1">
                            {#if replacementsActive && replacementsPaused}
                                Pausing (letting in-flight uploads finish)…
                            {:else if replacementsActive}
                                Uploading…
                            {:else if replPending > 0 || replFailed > 0}
                                Paused
                            {:else}
                                Finished
                            {/if}
                        </div>
                        <div class="progress mb-2" role="progressbar">
                            <div
                                class="progress-bar"
                                class:progress-bar-striped={replacementsActive}
                                class:progress-bar-animated={replacementsActive &&
                                    !replacementsPaused}
                                style="width: {replPct}%"
                            >
                                {replPct}%
                            </div>
                        </div>
                        <div class="small text-body-secondary mb-3">
                            <strong>{replCommitted.toLocaleString()}</strong> uploaded
                            · <strong>{replFailed.toLocaleString()}</strong> failed
                            · <strong>{replPending.toLocaleString()}</strong> pending
                            · {replTotal.toLocaleString()} total
                        </div>

                        <div class="d-flex gap-2 mb-3 flex-wrap">
                            {#if replacementsActive && !replacementsPaused}
                                <button
                                    type="button"
                                    class="btn btn-outline-secondary btn-sm"
                                    disabled={pausingReplacements}
                                    onclick={pauseReplacementsAction}
                                >
                                    {pausingReplacements ? "Pausing…" : "Pause"}
                                </button>
                            {/if}
                            {#if canResume}
                                <button
                                    type="button"
                                    class="btn btn-primary btn-sm"
                                    onclick={resumeReplacementsAction}
                                >
                                    Resume
                                </button>
                            {/if}
                            {#if !replacementsActive}
                                <button
                                    type="button"
                                    class="btn btn-outline-danger btn-sm"
                                    disabled={clearingReplacements}
                                    onclick={clearReplacementsAction}
                                >
                                    {clearingReplacements
                                        ? "Clearing…"
                                        : "Clear batch"}
                                </button>
                            {/if}
                        </div>

                        {#if replacementsError}
                            <div class="alert alert-danger small" role="alert">
                                {replacementsError}
                            </div>
                        {/if}

                        <div class="table-responsive">
                            <table class="table table-sm align-middle">
                                <thead>
                                    <tr>
                                        <th style="width: 90px;">Image</th>
                                        <th>Replacement file</th>
                                        <th style="width: 200px;">Status</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {#each sortedReplacementsItems as item (item.image_id)}
                                        <tr>
                                            <td>#{item.image_id}</td>
                                            <td class="small text-break">
                                                {fileName(item.file_path)}
                                            </td>
                                            <td>
                                                {#if activeUploads.has(item.image_id)}
                                                    <span
                                                        class="badge text-bg-primary"
                                                    >
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
                                                        >Uploaded</span
                                                    >
                                                {:else if item.status === "failed"}
                                                    <span
                                                        class="badge text-bg-danger"
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
                    {/if}
                </div>
            </div>
        </div>
    {/if}
{/if}
