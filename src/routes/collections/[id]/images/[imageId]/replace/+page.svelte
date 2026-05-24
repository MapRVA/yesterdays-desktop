<script lang="ts">
    import { page } from "$app/state";
    import { goto } from "$app/navigation";
    import { auth } from "$lib/auth.svelte";
    import { invoke, convertFileSrc } from "@tauri-apps/api/core";
    import { open as openDialog } from "@tauri-apps/plugin-dialog";

    type ImageDetail = {
        id: number;
        title: string | null;
        thumbnail: string | null;
        collection: { id: number; name: string };
    };

    let collectionId = $derived(page.params.id!);
    let imageId = $derived(page.params.imageId!);

    let image: ImageDetail | null = $state(null);
    let loading = $state(true);
    let loadError = $state("");

    let filePath: string | null = $state(null);
    let previewPath: string | null = $state(null);
    let previewState: "idle" | "generating" | "done" | "error" =
        $state("idle");
    let previewError = $state("");
    let uploadState: "idle" | "uploading" | "done" | "error" = $state("idle");
    let uploadError = $state("");

    $effect(() => {
        if (!auth.hydrated) return;
        if (!auth.user) {
            goto("/", { replaceState: true });
            return;
        }
        const iid = imageId;
        void load(iid);
    });

    async function load(iid: string) {
        loading = true;
        loadError = "";
        image = null;
        try {
            const resp = await fetch(
                `${auth.instanceUrl}/api/v2/images/${iid}/`,
                {
                    headers: auth.accessToken
                        ? { Authorization: `Bearer ${auth.accessToken}` }
                        : {},
                },
            );
            if (!resp.ok)
                throw new Error(`Image: server returned ${resp.status}`);
            image = await resp.json();
        } catch (e) {
            loadError = String(e);
        } finally {
            loading = false;
        }
    }

    async function pickFile() {
        const selected = await openDialog({
            directory: false,
            multiple: false,
            title: "Choose replacement file",
            filters: [
                {
                    name: "Images",
                    extensions: [
                        "jpg",
                        "jpeg",
                        "jpe",
                        "png",
                        "webp",
                        "gif",
                        "bmp",
                        "tif",
                        "tiff",
                    ],
                },
            ],
        });
        if (typeof selected === "string") {
            filePath = selected;
            previewPath = null;
            previewError = "";
            uploadState = "idle";
            uploadError = "";
            previewState = "generating";
            try {
                previewPath = await invoke<string>(
                    "preview_replacement_file",
                    { filePath: selected },
                );
                previewState = "done";
            } catch (e) {
                previewState = "error";
                previewError = String(e);
            }
        }
    }

    let filename = $derived.by(() => {
        const fp = filePath;
        if (!fp) return null;
        return fp.split(/[\\/]/).pop() || fp;
    });

    async function upload() {
        if (!image || !filePath || !auth.accessToken) return;
        uploadState = "uploading";
        uploadError = "";
        try {
            await invoke("replace_single_image", {
                instanceUrl: auth.instanceUrl,
                accessToken: auth.accessToken,
                refreshToken: auth.refreshToken ?? "",
                collectionId: image.collection.id,
                imageId: image.id,
                filePath,
            });
            uploadState = "done";
        } catch (e) {
            uploadState = "error";
            uploadError = String(e);
        }
    }

    function reset() {
        filePath = null;
        previewPath = null;
        previewState = "idle";
        previewError = "";
        uploadState = "idle";
        uploadError = "";
    }
</script>

<nav aria-label="breadcrumb">
    <ol class="breadcrumb">
        <li class="breadcrumb-item"><a href="/browse">Sources</a></li>
        {#if image}
            <li class="breadcrumb-item">
                <a href="/collections/{collectionId}">{image.collection.name}</a>
            </li>
            <li class="breadcrumb-item">
                <a href="/collections/{collectionId}/images/{image.id}">
                    {image.title ?? `Image #${image.id}`}
                </a>
            </li>
        {/if}
        <li class="breadcrumb-item active" aria-current="page">Replace</li>
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
{:else if image}
    <h1 class="mb-4">Replace image</h1>

    <div class="card shadow-sm mb-4">
        <div class="card-body">
            <h2 class="h5 mb-3">1. Choose replacement file</h2>
            <button
                type="button"
                class="btn btn-outline-primary"
                onclick={pickFile}
                disabled={uploadState === "uploading"}
            >
                {filePath ? "Change file…" : "Choose file…"}
            </button>
            {#if filePath}
                <div class="small text-body-secondary text-break mt-3 mb-0">
                    <code>{filePath}</code>
                </div>
            {/if}
        </div>
    </div>

    {#if filePath}
        <div class="card shadow-sm mb-4">
            <div class="card-body">
                <h2 class="h5 mb-3">2. Preview</h2>
                <div class="row g-3">
                    <div class="col-md-6">
                        <div class="small text-body-secondary mb-2">Before</div>
                        <div
                            class="bg-body-tertiary d-flex align-items-center justify-content-center"
                            style="min-height: 240px;"
                        >
                            {#if image.thumbnail}
                                <img
                                    src={image.thumbnail}
                                    alt="Current"
                                    class="img-fluid"
                                    style="object-fit: contain; max-height: 400px;"
                                />
                            {:else}
                                <span class="text-body-secondary small p-4">
                                    No thumbnail
                                </span>
                            {/if}
                        </div>
                    </div>
                    <div class="col-md-6">
                        <div class="small text-body-secondary mb-2">
                            After <span class="text-muted">({filename})</span>
                        </div>
                        <div
                            class="bg-body-tertiary d-flex align-items-center justify-content-center"
                            style="min-height: 240px;"
                        >
                            {#if previewState === "generating"}
                                <div
                                    class="d-flex align-items-center text-body-secondary small p-4"
                                >
                                    <span
                                        class="spinner-border spinner-border-sm me-2"
                                        role="status"
                                        aria-hidden="true"
                                    ></span>
                                    Generating preview…
                                </div>
                            {:else if previewState === "done" && previewPath}
                                <img
                                    src={convertFileSrc(previewPath)}
                                    alt="Replacement"
                                    class="img-fluid"
                                    style="object-fit: contain; max-height: 400px;"
                                />
                            {:else if previewState === "error"}
                                <div class="text-danger small p-4">
                                    Preview failed: {previewError}
                                </div>
                            {/if}
                        </div>
                    </div>
                </div>
            </div>
        </div>

        <div class="card shadow-sm mb-4">
            <div class="card-body">
                <h2 class="h5 mb-3">3. Upload</h2>
                {#if uploadState === "idle"}
                    <p class="text-body-secondary small mb-3">
                        This will replace the current image file for
                        <strong>#{image.id}</strong>. The image's title,
                        description, and other metadata will be unchanged.
                    </p>
                    <button
                        type="button"
                        class="btn btn-primary"
                        onclick={upload}
                        disabled={previewState === "generating"}
                    >
                        Upload replacement
                    </button>
                {:else if uploadState === "uploading"}
                    <div class="d-flex align-items-center text-body-secondary">
                        <span
                            class="spinner-border spinner-border-sm me-2"
                            role="status"
                            aria-hidden="true"
                        ></span>
                        Uploading…
                    </div>
                {:else if uploadState === "done"}
                    <div class="alert alert-success mb-3" role="alert">
                        Replacement uploaded successfully.
                    </div>
                    <div class="d-flex gap-2">
                        <a
                            class="btn btn-primary"
                            href="/collections/{collectionId}/images/{image.id}"
                        >
                            Back to image
                        </a>
                        <button
                            type="button"
                            class="btn btn-outline-secondary"
                            onclick={reset}
                        >
                            Replace again
                        </button>
                    </div>
                {:else if uploadState === "error"}
                    <div class="alert alert-danger mb-3" role="alert">
                        {uploadError}
                    </div>
                    <button
                        type="button"
                        class="btn btn-primary"
                        onclick={upload}
                    >
                        Retry upload
                    </button>
                {/if}
            </div>
        </div>
    {/if}
{/if}
