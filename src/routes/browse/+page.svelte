<script lang="ts">
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
    collections_url: string;
  };

  let sources: Source[] = $state([]);
  let loading = $state(true);
  let error = $state("");

  $effect(() => {
    if (!auth.hydrated) return;
    if (!auth.user) {
      goto("/", { replaceState: true });
      return;
    }
    void loadSources();
  });

  async function loadSources() {
    loading = true;
    error = "";
    try {
      const resp = await fetch(`${auth.instanceUrl}/api/v2/sources/`, {
        headers: auth.accessToken
          ? { Authorization: `Bearer ${auth.accessToken}` }
          : {},
      });
      if (!resp.ok) throw new Error(`Server returned ${resp.status}`);
      const data = await resp.json();
      sources = data.results;
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }
</script>

<div class="d-flex justify-content-between align-items-center mb-4">
  <h1 class="mb-0">Sources</h1>
  {#if auth.user?.can_import}
    <a href="/browse/new" class="btn btn-primary">Create New Source</a>
  {/if}
</div>

{#if loading}
  <div class="d-flex align-items-center text-body-secondary">
    <span class="spinner-border spinner-border-sm me-2" role="status" aria-hidden="true"></span>
    Loading sources…
  </div>
{:else if error}
  <div class="alert alert-danger" role="alert">{error}</div>
{:else if sources.length === 0}
  <p class="text-body-secondary">No sources found.</p>
{:else}
  <div class="table-responsive">
    <table class="table table-hover align-middle">
      <thead>
        <tr>
          <th scope="col">Source</th>
          <th scope="col" class="text-end">Collections</th>
          <th scope="col" class="text-end">Images</th>
        </tr>
      </thead>
      <tbody>
        {#each sources as src (src.id)}
          <tr>
            <td class="position-relative">
              <a href="/browse/{src.id}" class="stretched-link text-reset text-decoration-none fw-semibold">
                {src.name}
              </a>
              {#if !src.public}
                <span class="badge text-bg-warning ms-2">Private</span>
              {/if}
              {#if src.description}
                <div class="text-body-secondary small">{src.description}</div>
              {/if}
            </td>
            <td class="text-end">{src.collection_count.toLocaleString()}</td>
            <td class="text-end">{src.image_count.toLocaleString()}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}
