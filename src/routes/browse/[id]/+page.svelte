<script lang="ts">
    import { page } from "$app/state";
    import { goto } from "$app/navigation";
    import { auth } from "$lib/auth.svelte";

    type Source = {
        id: number;
        name: string;
        slug: string;
        url: string;
        description: string;
        public: boolean;
        collection_count: number;
        image_count: number;
    };

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

    let sourceId = $derived(page.params.id!);
    let source: Source | null = $state(null);
    let collections: Collection[] = $state([]);
    let loading = $state(true);
    let error = $state("");

    $effect(() => {
        if (!auth.hydrated) return;
        if (!auth.user) {
            goto("/", { replaceState: true });
            return;
        }
        const id = sourceId;
        void load(id);
    });

    async function load(id: string) {
        loading = true;
        error = "";
        source = null;
        collections = [];
        try {
            const headers: HeadersInit = auth.accessToken
                ? { Authorization: `Bearer ${auth.accessToken}` }
                : {};
            const [srcResp, colResp] = await Promise.all([
                fetch(`${auth.instanceUrl}/api/v2/sources/${id}/`, { headers }),
                fetch(
                    `${auth.instanceUrl}/api/v2/sources/${id}/collections/`,
                    { headers },
                ),
            ]);
            if (!srcResp.ok)
                throw new Error(`Source: server returned ${srcResp.status}`);
            if (!colResp.ok)
                throw new Error(
                    `Collections: server returned ${colResp.status}`,
                );
            source = await srcResp.json();
            const data = await colResp.json();
            collections = data.results;
        } catch (e) {
            error = String(e);
        } finally {
            loading = false;
        }
    }
</script>

<nav aria-label="breadcrumb">
    <ol class="breadcrumb">
        <li class="breadcrumb-item"><a href="/browse">Sources</a></li>
        <li class="breadcrumb-item active" aria-current="page">
            {source?.name ?? "…"}
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
{:else if source}
    <div class="mb-4">
        <h1 class="mb-2">
            {source.name}
            {#if !source.public}
                <span class="badge text-bg-warning align-middle fs-6"
                    >Private</span
                >
            {/if}
        </h1>
        {#if source.description}
            <p class="text-body-secondary mb-2">{source.description}</p>
        {/if}
        {#if source.url}
            <a
                href={source.url}
                target="_blank"
                rel="noopener noreferrer"
                class="small"
            >
                {source.url}
            </a>
        {/if}
    </div>

    <div class="d-flex justify-content-between align-items-center mb-3">
        <h2 class="mb-0 h4">Collections</h2>
        {#if auth.user?.can_import}
            <a href="/browse/{source.id}/new" class="btn btn-primary"
                >Create New Collection</a
            >
        {/if}
    </div>

    {#if collections.length === 0}
        <p class="text-body-secondary">No collections in this source.</p>
    {:else}
        <div class="table-responsive">
            <table class="table table-hover align-middle">
                <thead>
                    <tr>
                        <th scope="col">Collection</th>
                        <th scope="col" class="text-end">Images</th>
                    </tr>
                </thead>
                <tbody>
                    {#each collections as col (col.id)}
                        <tr
                            role="button"
                            onclick={() => goto(`/collections/${col.id}`)}
                        >
                            <td>
                                <div class="fw-semibold">
                                    {col.name}
                                    {#if !col.public}
                                        <span
                                            class="badge text-bg-warning ms-2"
                                            >Private</span
                                        >
                                    {/if}
                                </div>
                                {#if col.description}
                                    <div class="text-body-secondary small">
                                        {col.description}
                                    </div>
                                {/if}
                            </td>
                            <td class="text-end"
                                >{col.image_count.toLocaleString()}</td
                            >
                        </tr>
                    {/each}
                </tbody>
            </table>
        </div>
    {/if}
{/if}
