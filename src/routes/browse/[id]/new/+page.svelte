<script lang="ts">
    import { page } from "$app/state";
    import { goto } from "$app/navigation";
    import { auth } from "$lib/auth.svelte";

    type Source = {
        id: number;
        name: string;
    };

    let sourceId = $derived(page.params.id!);
    let source: Source | null = $state(null);
    let sourceLoading = $state(true);
    let sourceError = $state("");

    let name = $state("");
    let url = $state("");
    let description = $state("");
    let isPublic = $state(false);

    let submitting = $state(false);
    let error = $state("");
    let fieldErrors: Record<string, string[]> = $state({});

    $effect(() => {
        if (!auth.hydrated) return;
        if (!auth.user) {
            goto("/", { replaceState: true });
            return;
        }
        void loadSource(sourceId);
    });

    async function loadSource(id: string) {
        sourceLoading = true;
        sourceError = "";
        try {
            const resp = await fetch(
                `${auth.instanceUrl}/api/v2/sources/${id}/`,
                {
                    headers: auth.accessToken
                        ? { Authorization: `Bearer ${auth.accessToken}` }
                        : {},
                },
            );
            if (!resp.ok) throw new Error(`Server returned ${resp.status}`);
            source = await resp.json();
        } catch (e) {
            sourceError = String(e);
        } finally {
            sourceLoading = false;
        }
    }

    async function submit(event: Event) {
        event.preventDefault();
        submitting = true;
        error = "";
        fieldErrors = {};
        try {
            const resp = await fetch(
                `${auth.instanceUrl}/api/v2/collections/`,
                {
                    method: "POST",
                    headers: {
                        "Content-Type": "application/json",
                        Authorization: `Bearer ${auth.accessToken}`,
                    },
                    body: JSON.stringify({
                        name,
                        source: Number(sourceId),
                        url,
                        description,
                        public: isPublic,
                    }),
                },
            );
            if (!resp.ok) {
                const body = await resp.json().catch(() => null);
                if (body && typeof body === "object" && !Array.isArray(body)) {
                    fieldErrors = body as Record<string, string[]>;
                }
                throw new Error(`Server returned ${resp.status}`);
            }
            goto(`/browse/${sourceId}`);
        } catch (e) {
            if (!error) error = String(e);
        } finally {
            submitting = false;
        }
    }
</script>

<nav aria-label="breadcrumb">
    <ol class="breadcrumb">
        <li class="breadcrumb-item"><a href="/browse">Sources</a></li>
        <li class="breadcrumb-item">
            <a href="/browse/{sourceId}">{source?.name ?? "…"}</a>
        </li>
        <li class="breadcrumb-item active" aria-current="page">
            New Collection
        </li>
    </ol>
</nav>

<h1 class="mb-4">Create New Collection</h1>

{#if sourceLoading}
    <div class="d-flex align-items-center text-body-secondary">
        <span
            class="spinner-border spinner-border-sm me-2"
            role="status"
            aria-hidden="true"
        ></span>
        Loading source…
    </div>
{:else if sourceError}
    <div class="alert alert-danger" role="alert">{sourceError}</div>
{:else if source}
    <form onsubmit={submit} class="col-md-8">
        <div class="mb-3">
            <label for="source-name" class="form-label">Source</label>
            <input
                id="source-name"
                type="text"
                class="form-control"
                value={source.name}
                disabled
            />
        </div>

        <div class="mb-3">
            <label for="collection-name" class="form-label">Name</label>
            <input
                id="collection-name"
                type="text"
                class="form-control"
                class:is-invalid={fieldErrors.name}
                bind:value={name}
                required
                disabled={submitting}
            />
            {#if fieldErrors.name}
                <div class="invalid-feedback">{fieldErrors.name.join(" ")}</div>
            {/if}
        </div>

        <div class="mb-3">
            <label for="collection-url" class="form-label"
                >URL <span class="text-body-secondary">(optional)</span></label
            >
            <input
                id="collection-url"
                type="url"
                class="form-control"
                class:is-invalid={fieldErrors.url}
                placeholder="https://example.org"
                bind:value={url}
                disabled={submitting}
            />
            {#if fieldErrors.url}
                <div class="invalid-feedback">{fieldErrors.url.join(" ")}</div>
            {/if}
        </div>

        <div class="mb-3">
            <label for="collection-description" class="form-label"
                >Description <span class="text-body-secondary"
                    >(optional)</span
                ></label
            >
            <textarea
                id="collection-description"
                class="form-control"
                class:is-invalid={fieldErrors.description}
                rows="4"
                bind:value={description}
                disabled={submitting}
            ></textarea>
            {#if fieldErrors.description}
                <div class="invalid-feedback">
                    {fieldErrors.description.join(" ")}
                </div>
            {/if}
        </div>

        <div class="mb-3 form-check">
            <input
                id="collection-public"
                type="checkbox"
                class="form-check-input"
                bind:checked={isPublic}
                disabled={submitting}
            />
            <label for="collection-public" class="form-check-label"
                >Public</label
            >
            <div class="form-text">
                If unchecked, this collection and its images are hidden from
                other users.
            </div>
            {#if fieldErrors.public}
                <div class="text-danger small">
                    {fieldErrors.public.join(" ")}
                </div>
            {/if}
        </div>

        {#if error}
            <div class="alert alert-danger" role="alert">{error}</div>
        {/if}

        <div class="d-flex gap-2">
            <button type="submit" class="btn btn-primary" disabled={submitting}>
                {#if submitting}
                    <span
                        class="spinner-border spinner-border-sm me-1"
                        role="status"
                        aria-hidden="true"
                    ></span>
                    Creating…
                {:else}
                    Create Collection
                {/if}
            </button>
            <a href="/browse/{sourceId}" class="btn btn-outline-secondary"
                >Cancel</a
            >
        </div>
    </form>
{/if}
