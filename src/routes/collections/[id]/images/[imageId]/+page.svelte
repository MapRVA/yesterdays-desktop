<script lang="ts">
    import { page } from "$app/state";
    import { goto } from "$app/navigation";
    import { auth } from "$lib/auth.svelte";

    type License = {
        display_name: string;
        permalink: string | null;
    };

    type SubjectSummary = {
        id: number;
        title: string;
        slug: string;
        wikidata: {
            wikidata_id: string;
            uri: string;
            title: string;
            description: string;
        } | null;
    };

    type Georeference = {
        id: number;
        latitude: number;
        longitude: number;
        direction: number | null;
        confidence: string | null;
        confidence_notes: string | null;
        georeferenced_by: string;
        georeferenced_at: string;
    };

    type Comment = {
        id: number;
        text: string;
        commented_by: string;
        created_at: string;
    };

    type ImageDetail = {
        id: number;
        title: string | null;
        permalink: string | null;
        thumbnail: string | null;
        original_url: string | null;
        description: string | null;
        creator: string | null;
        license: License | null;
        original_date: string | null;
        edtf_date: string | null;
        date_display: string;
        collection: {
            id: number;
            name: string;
            slug: string;
            source_name: string;
        };
        subjects: SubjectSummary[];
        from_above: boolean;
        duplicate_of: number | null;
        scale: string | null;
        mirror: "none" | "h" | "v";
        rotation: 0 | 90 | 180 | 270;
        georeference_status: string;
        georeferences: Georeference[];
        from_above_georeferences: unknown[];
        comments: Comment[];
        detail_url: string | null;
        iiif_url: string | null;
    };

    type Collection = {
        id: number;
        name: string;
        slug: string;
        source: { id: number; name: string; slug: string };
        public: boolean;
    };

    let collectionId = $derived(page.params.id!);
    let imageId = $derived(page.params.imageId!);
    let image: ImageDetail | null = $state(null);
    let collection: Collection | null = $state(null);
    let loading = $state(true);
    let error = $state("");
    let imgFailed = $state(false);

    let actionsOpen = $state(false);
    let actionsContainer: HTMLElement | undefined = $state();

    $effect(() => {
        if (!actionsOpen) return;
        const handler = (e: MouseEvent) => {
            if (
                actionsContainer &&
                !actionsContainer.contains(e.target as Node)
            ) {
                actionsOpen = false;
            }
        };
        window.addEventListener("mousedown", handler);
        return () => window.removeEventListener("mousedown", handler);
    });

    $effect(() => {
        if (!auth.hydrated) return;
        if (!auth.user) {
            goto("/", { replaceState: true });
            return;
        }
        const cid = collectionId;
        const iid = imageId;
        void load(cid, iid);
    });

    function authHeaders(): HeadersInit {
        return auth.accessToken
            ? { Authorization: `Bearer ${auth.accessToken}` }
            : {};
    }

    async function load(cid: string, iid: string) {
        loading = true;
        error = "";
        image = null;
        collection = null;
        imgFailed = false;
        try {
            const [imgResp, colResp] = await Promise.all([
                fetch(`${auth.instanceUrl}/api/v2/images/${iid}/`, {
                    headers: authHeaders(),
                }),
                fetch(`${auth.instanceUrl}/api/v2/collections/${cid}/`, {
                    headers: authHeaders(),
                }),
            ]);
            if (!imgResp.ok)
                throw new Error(`Image: server returned ${imgResp.status}`);
            if (!colResp.ok)
                throw new Error(
                    `Collection: server returned ${colResp.status}`,
                );
            image = await imgResp.json();
            collection = await colResp.json();
        } catch (e) {
            error = String(e);
        } finally {
            loading = false;
        }
    }

    function statusLabel(status: string): string {
        switch (status) {
            case "georeferenced":
                return "Georeferenced";
            case "duplicate":
                return "Duplicate";
            case "will_not_georef":
                return "Will not georeference";
            case "available":
                return "Available";
            default:
                return status;
        }
    }

    function statusBadgeClass(status: string): string {
        switch (status) {
            case "georeferenced":
                return "text-bg-success";
            case "duplicate":
                return "text-bg-secondary";
            case "will_not_georef":
                return "text-bg-dark";
            case "available":
                return "text-bg-info";
            default:
                return "text-bg-light";
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
            {image?.title ?? `Image #${imageId}`}
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
{:else if image}
    <div class="mb-4">
        <div class="d-flex justify-content-between align-items-start gap-3">
            <h1 class="mb-2">
                {image.title ?? `Image #${image.id}`}
                <span class="text-body-secondary fs-5 fw-normal">
                    #{image.id}
                </span>
            </h1>
            <div class="dropdown" bind:this={actionsContainer}>
                <button
                    type="button"
                    class="btn btn-outline-primary dropdown-toggle"
                    aria-expanded={actionsOpen}
                    onclick={() => (actionsOpen = !actionsOpen)}
                >
                    Actions
                </button>
                <ul
                    class="dropdown-menu dropdown-menu-end"
                    class:show={actionsOpen}
                >
                    <li>
                        <a
                            class="dropdown-item"
                            href="/collections/{collectionId}/images/{image.id}/replace"
                        >
                            Replace image
                        </a>
                    </li>
                </ul>
            </div>
        </div>
        <div class="d-flex flex-wrap gap-2 mb-2">
            <span
                class="badge {statusBadgeClass(image.georeference_status)}"
            >
                {statusLabel(image.georeference_status)}
            </span>
            {#if image.from_above}
                <span class="badge text-bg-primary">From above</span>
            {/if}
            {#if image.duplicate_of}
                <span class="badge text-bg-secondary">
                    Duplicate of #{image.duplicate_of}
                </span>
            {/if}
        </div>
    </div>

    <div class="row g-4">
        <div class="col-lg-5">
            <div class="card shadow-sm">
                <div
                    class="bg-body-tertiary d-flex align-items-center justify-content-center"
                    style="min-height: 320px;"
                >
                    {#if image.thumbnail && !imgFailed}
                        <img
                            src={image.thumbnail}
                            alt={image.title ?? `Image #${image.id}`}
                            class="img-fluid w-100"
                            style="object-fit: contain; max-height: 600px;"
                            onerror={() => (imgFailed = true)}
                        />
                    {:else}
                        <span class="text-body-secondary small p-4">
                            Thumbnail unavailable
                        </span>
                    {/if}
                </div>
                <div class="card-body p-3">
                    <div class="d-flex flex-column gap-1 small">
                        <a
                            href={`${auth.instanceUrl}/${image.id}/`}
                            target="_blank"
                            rel="noopener noreferrer"
                        >
                            Public page ↗
                        </a>
                        {#if image.permalink}
                            <a
                                href={image.permalink}
                                target="_blank"
                                rel="noopener noreferrer"
                            >
                                Image file ↗
                            </a>
                        {/if}
                        {#if image.original_url}
                            <a
                                href={image.original_url}
                                target="_blank"
                                rel="noopener noreferrer"
                            >
                                Original source ↗
                            </a>
                        {/if}
                        {#if image.iiif_url}
                            <a
                                href={image.iiif_url}
                                target="_blank"
                                rel="noopener noreferrer"
                            >
                                IIIF manifest ↗
                            </a>
                        {/if}
                    </div>
                </div>
            </div>
        </div>

        <div class="col-lg-7">
            {#if image.description}
                <div class="mb-4">
                    <h2 class="h5">Description</h2>
                    <p class="mb-0" style="white-space: pre-wrap;">
                        {image.description}
                    </p>
                </div>
            {/if}

            <div class="mb-4">
                <h2 class="h5">Details</h2>
                <dl class="row mb-0">
                    <dt class="col-sm-4">Date</dt>
                    <dd class="col-sm-8">{image.date_display}</dd>

                    {#if image.edtf_date}
                        <dt class="col-sm-4">EDTF</dt>
                        <dd class="col-sm-8"><code>{image.edtf_date}</code></dd>
                    {/if}

                    {#if image.original_date}
                        <dt class="col-sm-4">Original date</dt>
                        <dd class="col-sm-8">{image.original_date}</dd>
                    {/if}

                    {#if image.creator}
                        <dt class="col-sm-4">Creator</dt>
                        <dd class="col-sm-8">{image.creator}</dd>
                    {/if}

                    {#if image.scale}
                        <dt class="col-sm-4">Scale</dt>
                        <dd class="col-sm-8">{image.scale}</dd>
                    {/if}

                    {#if image.rotation !== 0 || image.mirror !== "none"}
                        <dt class="col-sm-4">Transform</dt>
                        <dd class="col-sm-8">
                            {image.rotation}°
                            {#if image.mirror !== "none"}
                                · mirror {image.mirror === "h"
                                    ? "horizontal"
                                    : "vertical"}
                            {/if}
                        </dd>
                    {/if}

                    {#if image.license}
                        <dt class="col-sm-4">License</dt>
                        <dd class="col-sm-8">
                            {#if image.license.permalink}
                                <a
                                    href={image.license.permalink}
                                    target="_blank"
                                    rel="noopener noreferrer"
                                >
                                    {image.license.display_name}
                                </a>
                            {:else}
                                {image.license.display_name}
                            {/if}
                        </dd>
                    {/if}
                </dl>
            </div>

            {#if image.subjects.length > 0}
                <div class="mb-4">
                    <h2 class="h5">Subjects</h2>
                    <div class="d-flex flex-wrap gap-2">
                        {#each image.subjects as subj (subj.id)}
                            <span class="badge text-bg-light border">
                                {subj.title}
                            </span>
                        {/each}
                    </div>
                </div>
            {/if}

            {#if image.georeferences.length > 0}
                <div class="mb-4">
                    <h2 class="h5">Georeferences</h2>
                    <ul class="list-unstyled mb-0">
                        {#each image.georeferences as gr (gr.id)}
                            <li class="small mb-1">
                                <code>
                                    {gr.latitude.toFixed(5)},
                                    {gr.longitude.toFixed(5)}
                                </code>
                                {#if gr.direction !== null}
                                    · {gr.direction}°
                                {/if}
                                · by {gr.georeferenced_by}
                            </li>
                        {/each}
                    </ul>
                </div>
            {/if}

            {#if image.from_above_georeferences.length > 0}
                <div class="mb-4">
                    <h2 class="h5">From-above georeferences</h2>
                    <p class="text-body-secondary small mb-0">
                        {image.from_above_georeferences.length} polygon{image
                            .from_above_georeferences.length === 1
                            ? ""
                            : "s"}
                    </p>
                </div>
            {/if}

            {#if image.comments.length > 0}
                <div class="mb-4">
                    <h2 class="h5">Comments</h2>
                    <ul class="list-unstyled mb-0">
                        {#each image.comments as c (c.id)}
                            <li class="mb-2">
                                <div class="small text-body-secondary">
                                    {c.commented_by}
                                </div>
                                <div style="white-space: pre-wrap;">
                                    {c.text}
                                </div>
                            </li>
                        {/each}
                    </ul>
                </div>
            {/if}
        </div>
    </div>
{/if}

<style>
    .dropdown-menu.show {
        right: 0;
        left: auto;
    }
</style>
