<script lang="ts">
    import { page } from "$app/state";
    import { goto } from "$app/navigation";
    import { auth } from "$lib/auth.svelte";
    import ImageCard, { type ImageCardData } from "$lib/ImageCard.svelte";

    type Collection = {
        id: number;
        name: string;
        slug: string;
        url: string;
        description: string;
        source: { id: number; name: string; slug: string };
        public: boolean;
        image_count: number;
        images_url: string;
    };

    type ImagesPage = {
        count: number;
        next: string | null;
        previous: string | null;
        results: ImageCardData[];
    };

    let collectionId = $derived(page.params.id!);
    let collection: Collection | null = $state(null);
    let images: ImageCardData[] = $state([]);
    let nextUrl: string | null = $state(null);
    let loading = $state(true);
    let loadingMore = $state(false);
    let error = $state("");

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
        const id = collectionId;
        void load(id);
    });

    function authHeaders(): HeadersInit {
        return auth.accessToken
            ? { Authorization: `Bearer ${auth.accessToken}` }
            : {};
    }

    async function load(id: string) {
        loading = true;
        error = "";
        collection = null;
        images = [];
        nextUrl = null;
        try {
            const [colResp, imgResp] = await Promise.all([
                fetch(`${auth.instanceUrl}/api/v2/collections/${id}/`, {
                    headers: authHeaders(),
                }),
                fetch(
                    `${auth.instanceUrl}/api/v2/images/?collection=${id}&page_size=24`,
                    { headers: authHeaders() },
                ),
            ]);
            if (!colResp.ok)
                throw new Error(`Collection: server returned ${colResp.status}`);
            if (!imgResp.ok)
                throw new Error(`Images: server returned ${imgResp.status}`);
            collection = await colResp.json();
            const imgData: ImagesPage = await imgResp.json();
            images = imgData.results;
            nextUrl = imgData.next;
        } catch (e) {
            error = String(e);
        } finally {
            loading = false;
        }
    }

    async function loadMore() {
        if (!nextUrl || loadingMore) return;
        loadingMore = true;
        try {
            const resp = await fetch(nextUrl, { headers: authHeaders() });
            if (!resp.ok) throw new Error(`Images: server returned ${resp.status}`);
            const data: ImagesPage = await resp.json();
            images = [...images, ...data.results];
            nextUrl = data.next;
        } catch (e) {
            error = String(e);
        } finally {
            loadingMore = false;
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
        {/if}
        <li class="breadcrumb-item active" aria-current="page">
            {collection?.name ?? "…"}
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
    <div class="mb-4">
        <div class="d-flex justify-content-between align-items-start gap-3">
            <h1 class="mb-2">
                {collection.name}
                {#if !collection.public}
                    <span class="badge text-bg-warning align-middle fs-6"
                        >Private</span
                    >
                {/if}
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
                            href="/collections/{collection.id}/import"
                        >
                            Import images
                        </a>
                    </li>
                    <li>
                        <a
                            class="dropdown-item"
                            href="/collections/{collection.id}/upload-replacements"
                        >
                            Upload higher-res replacements
                        </a>
                    </li>
                </ul>
            </div>
        </div>
        {#if collection.description}
            <p class="text-body-secondary mb-2">{collection.description}</p>
        {/if}
        {#if collection.url}
            <a
                href={collection.url}
                target="_blank"
                rel="noopener noreferrer"
                class="small"
            >
                {collection.url}
            </a>
        {/if}
    </div>

    <div class="d-flex justify-content-between align-items-center mb-3">
        <h2 class="mb-0 h4">
            Images
            <span class="text-body-secondary fs-6 fw-normal">
                ({collection.image_count.toLocaleString()})
            </span>
        </h2>
    </div>

    {#if images.length === 0}
        <p class="text-body-secondary">No images in this collection.</p>
    {:else}
        <div class="row">
            {#each images as img (img.id)}
                <ImageCard
                    image={img}
                    href="/collections/{collectionId}/images/{img.id}"
                />
            {/each}
        </div>

        {#if nextUrl}
            <div class="d-flex justify-content-center mb-4">
                <button
                    type="button"
                    class="btn btn-outline-primary"
                    disabled={loadingMore}
                    onclick={loadMore}
                >
                    {#if loadingMore}
                        <span
                            class="spinner-border spinner-border-sm me-2"
                            role="status"
                            aria-hidden="true"
                        ></span>
                        Loading…
                    {:else}
                        Load more
                    {/if}
                </button>
            </div>
        {/if}
    {/if}
{/if}

<style>
    .dropdown-menu.show {
        right: 0;
        left: auto;
    }
</style>
