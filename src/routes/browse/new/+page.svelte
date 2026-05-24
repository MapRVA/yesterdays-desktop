<script lang="ts">
    import { goto } from "$app/navigation";
    import { auth } from "$lib/auth.svelte";

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
        }
    });

    async function submit(event: Event) {
        event.preventDefault();
        submitting = true;
        error = "";
        fieldErrors = {};
        try {
            const resp = await fetch(`${auth.instanceUrl}/api/v2/sources/`, {
                method: "POST",
                headers: {
                    "Content-Type": "application/json",
                    Authorization: `Bearer ${auth.accessToken}`,
                },
                body: JSON.stringify({
                    name,
                    url,
                    description,
                    public: isPublic,
                }),
            });
            if (!resp.ok) {
                const body = await resp.json().catch(() => null);
                if (body && typeof body === "object" && !Array.isArray(body)) {
                    fieldErrors = body as Record<string, string[]>;
                }
                throw new Error(`Server returned ${resp.status}`);
            }
            goto("/browse");
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
        <li class="breadcrumb-item active" aria-current="page">New</li>
    </ol>
</nav>

<h1 class="mb-4">Create New Source</h1>

<form onsubmit={submit} class="col-md-8">
    <div class="mb-3">
        <label for="source-name" class="form-label">Name</label>
        <input
            id="source-name"
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
        <label for="source-url" class="form-label">URL</label>
        <input
            id="source-url"
            type="url"
            class="form-control"
            class:is-invalid={fieldErrors.url}
            placeholder="https://example.org"
            bind:value={url}
            required
            disabled={submitting}
        />
        {#if fieldErrors.url}
            <div class="invalid-feedback">{fieldErrors.url.join(" ")}</div>
        {/if}
    </div>

    <div class="mb-3">
        <label for="source-description" class="form-label">Description</label>
        <textarea
            id="source-description"
            class="form-control"
            class:is-invalid={fieldErrors.description}
            rows="4"
            bind:value={description}
            required
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
            id="source-public"
            type="checkbox"
            class="form-check-input"
            bind:checked={isPublic}
            disabled={submitting}
        />
        <label for="source-public" class="form-check-label">Public</label>
        <div class="form-text">
            If unchecked, this source and its images are hidden from other
            users.
        </div>
        {#if fieldErrors.public}
            <div class="text-danger small">{fieldErrors.public.join(" ")}</div>
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
                Create Source
            {/if}
        </button>
        <a href="/browse" class="btn btn-outline-secondary">Cancel</a>
    </div>
</form>
