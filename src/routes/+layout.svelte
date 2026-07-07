<script lang="ts">
    import "../styles.scss";
    import { onMount } from "svelte";
    import type { Snippet } from "svelte";
    import { auth } from "$lib/auth.svelte";

    let { children }: { children: Snippet } = $props();

    onMount(() => {
        void auth.hydrate();
    });

    let dropdownOpen = $state(false);
    let dropdownContainer: HTMLElement | undefined = $state();

    $effect(() => {
        if (!dropdownOpen) return;
        const handler = (e: MouseEvent) => {
            if (
                dropdownContainer &&
                !dropdownContainer.contains(e.target as Node)
            ) {
                dropdownOpen = false;
            }
        };
        window.addEventListener("mousedown", handler);
        return () => window.removeEventListener("mousedown", handler);
    });
</script>

<nav class="navbar bg-primary" data-bs-theme="dark">
    <div class="container-fluid">
        <a href="/" class="navbar-brand mb-0 h1 text-decoration-none">Yesterdays Desktop Importer</a>
        {#if auth.user}
            <div class="dropdown" bind:this={dropdownContainer}>
                <button
                    type="button"
                    class="btn btn-sm btn-dark dropdown-toggle"
                    aria-expanded={dropdownOpen}
                    onclick={() => (dropdownOpen = !dropdownOpen)}
                >
                    {auth.user.username}
                </button>
                <ul
                    class="dropdown-menu dropdown-menu-end"
                    class:show={dropdownOpen}
                >
                    <li class="px-3 py-1">
                        <div class="text-body-secondary small">
                            Connected to
                        </div>
                        <div><strong>{auth.instanceName}</strong></div>
                    </li>
                    <li><hr class="dropdown-divider" /></li>
                    <li class="px-3 py-1 small">
                        <div>
                            <strong>Staff:</strong>
                            {auth.user.is_staff ? "Yes" : "No"}
                        </div>
                        <div>
                            <strong>Can import:</strong>
                            {auth.user.can_import ? "Yes" : "No"}
                        </div>
                        <div>
                            <strong>Scopes:</strong>
                            {auth.user.scopes.join(", ")}
                        </div>
                    </li>
                    {#if !auth.user.can_import}
                        <li><hr class="dropdown-divider" /></li>
                        <li class="px-3 py-1">
                            <div class="text-warning small">
                                Your account does not have import
                                permissions. Contact the instance admin.
                            </div>
                        </li>
                    {/if}
                    <li><hr class="dropdown-divider" /></li>
                    <li>
                        <button
                            type="button"
                            class="dropdown-item"
                            onclick={() => auth.disconnect()}
                        >
                            Log out
                        </button>
                    </li>
                </ul>
            </div>
        {/if}
    </div>
</nav>

<main class="container mt-4">
    {@render children()}
</main>

<style>
    .dropdown-menu.show {
        right: 0;
        left: auto;
    }
    main {
        will-change: transform;
    }
</style>
