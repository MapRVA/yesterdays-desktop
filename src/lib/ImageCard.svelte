<script module lang="ts">
    export type ImageCardData = {
        id: number;
        title: string | null;
        thumbnail: string | null;
        date_display: string;
        from_above: boolean;
        duplicate_of: number | null;
        georeference_status:
            | "georeferenced"
            | "duplicate"
            | "will_not_georef"
            | "available"
            | string;
    };
</script>

<script lang="ts">
    let { image, href }: { image: ImageCardData; href?: string } = $props();

    let displayTitle = $derived(
        image.title && image.title.length > 50
            ? image.title.slice(0, 50) + "…"
            : (image.title ?? `Image #${image.id}`),
    );

    let hasDate = $derived(image.date_display && image.date_display !== "Unknown date");

    let imgFailed = $state(false);
</script>

{#snippet card()}
    <div class="card h-100 shadow-sm">
        <div class="position-relative">
            <div
                class="image-container d-block bg-body-tertiary"
                style="height: 200px; overflow: hidden;"
            >
                {#if image.thumbnail && !imgFailed}
                    <img
                        src={image.thumbnail}
                        alt={image.title ?? `Image #${image.id}`}
                        class="img-fluid w-100 h-100"
                        style="object-fit: cover;"
                        loading="lazy"
                        decoding="async"
                        onerror={() => (imgFailed = true)}
                    />
                {:else}
                    <div
                        class="d-flex align-items-center justify-content-center h-100 text-body-secondary small"
                    >
                        Image unavailable
                    </div>
                {/if}
            </div>
        </div>

        <div class="card-body p-3">
            <h6 class="card-title mb-2">{displayTitle}</h6>
            {#if hasDate}
                <p class="text-body-secondary small mb-0">
                    {image.date_display}
                </p>
            {/if}
        </div>
    </div>
{/snippet}

<div class="col-lg-3 col-md-4 col-sm-6 mb-4 image-card-col">
    {#if href}
        <a class="card-link" {href}>{@render card()}</a>
    {:else}
        {@render card()}
    {/if}
</div>

<style>
    .image-card-col {
        content-visibility: auto;
        contain-intrinsic-size: auto 320px;
    }
    .card-link {
        display: block;
        height: 100%;
        color: inherit;
        text-decoration: none;
    }
    .card-link:hover .card,
    .card-link:focus-visible .card {
        box-shadow: 0 0.5rem 1rem rgba(0, 0, 0, 0.15) !important;
    }
    .card-link .card {
        transition: box-shadow 0.15s ease;
    }
</style>
