<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { auth, type User } from "$lib/auth.svelte";

  let connectError = $state("");
  let connectLoading = $state(false);

  let authError = $state("");
  let authLoading = $state(false);
  let cancelRequested = $state(false);

  async function connect(event: Event) {
    event.preventDefault();
    connectError = "";
    connectLoading = true;

    try {
      const result: { url: string; name: string } = await invoke("validate_instance", {
        url: auth.instanceUrl,
      });
      auth.instanceUrl = result.url;
      auth.instanceName = result.name;
      auth.connected = true;
    } catch (e) {
      connectError = String(e);
    } finally {
      connectLoading = false;
    }
  }

  async function login() {
    authError = "";
    authLoading = true;
    cancelRequested = false;

    try {
      const result: {
        user: User;
        tokens: { access_token: string; refresh_token: string | null; expires_in: number };
      } = await invoke("start_oauth_login", { instanceUrl: auth.instanceUrl });
      await auth.setSession({
        user: result.user,
        accessToken: result.tokens.access_token,
        refreshToken: result.tokens.refresh_token,
        expiresIn: result.tokens.expires_in,
      });
    } catch (e) {
      if (!cancelRequested) {
        authError = String(e);
      }
    } finally {
      authLoading = false;
      cancelRequested = false;
    }
  }

  async function cancelLogin() {
    cancelRequested = true;
    try {
      await invoke("cancel_oauth_login");
    } catch {
      // Ignore — the in-flight start_oauth_login call will surface any real failure
    }
  }

  // Whether the user picked "Connect to an Instance" from the signed-out
  // landing. The landing stays the default so preparing a folder offline is
  // always one click away, even when logged out.
  let connecting = $state(false);

  function startConnect() {
    connectError = "";
    connecting = true;
  }

  function cancelConnect() {
    connectError = "";
    connecting = false;
  }

  async function backFromLogin() {
    connecting = false;
    await auth.disconnect();
  }
</script>

{#if !auth.hydrated}
  <div class="row justify-content-center mt-5">
    <div class="col-md-6 text-center text-body-secondary">
      <span class="spinner-border spinner-border-sm me-2" role="status" aria-hidden="true"></span>
      Loading…
    </div>
  </div>

{:else if auth.user}
  <!-- Landing (signed in): choose an activity -->
  <div class="text-center mt-5 mb-4">
    <h1 class="mb-2">Yesterdays Desktop Importer</h1>
    <p class="text-body-secondary mb-0">
      Connected to <strong>{auth.instanceName}</strong> as {auth.user?.username}.
    </p>
  </div>

  <div class="row justify-content-center g-4 mb-4">
    <div class="col-sm-6 col-lg-5">
      <a
        href="/prepare-import"
        class="card h-100 text-reset text-decoration-none shadow-sm landing-card"
      >
        <div class="card-body text-center p-4 p-lg-5">
          <div class="landing-icon mb-3" aria-hidden="true">🗂️</div>
          <h2 class="h4 mb-2">Prepare an Image Folder</h2>
          <p class="text-body-secondary mb-0">
            Reorder, tag, and export a folder of images into an import-ready series with JSON sidecars.
          </p>
        </div>
      </a>
    </div>
    <div class="col-sm-6 col-lg-5">
      <a
        href="/browse"
        class="card h-100 text-reset text-decoration-none shadow-sm landing-card"
      >
        <div class="card-body text-center p-4 p-lg-5">
          <div class="landing-icon mb-3" aria-hidden="true">🔍</div>
          <h2 class="h4 mb-2">Browse {auth.instanceName}</h2>
          <p class="text-body-secondary mb-0">
            Explore sources, collections, and images on this instance.
          </p>
        </div>
      </a>
    </div>
  </div>

{:else if auth.connected}
  <!-- Log in to the connected instance -->
  <div class="row justify-content-center mt-5">
    <div class="col-md-6">
      <div class="card">
        <div class="card-body">
          <h5 class="card-title">Log In</h5>
          <p class="card-text text-body-secondary">
            Connected to <strong>{auth.instanceName}</strong>. Log in with your OpenStreetMap account to continue.
          </p>

          {#if authError}
            <div class="alert alert-danger" role="alert">{authError}</div>
          {/if}

          <div class="d-flex gap-2">
            <button class="btn btn-primary" onclick={login} disabled={authLoading}>
              {#if authLoading}
                <span class="spinner-border spinner-border-sm me-1" role="status" aria-hidden="true"></span>
                Waiting for browser...
              {:else}
                Log in with OpenStreetMap
              {/if}
            </button>
            {#if authLoading}
              <button class="btn btn-outline-secondary" onclick={cancelLogin} disabled={cancelRequested}>
                Cancel
              </button>
            {:else}
              <button class="btn btn-outline-secondary" onclick={backFromLogin}>
                Back
              </button>
            {/if}
          </div>
        </div>
      </div>
    </div>
  </div>

{:else if connecting}
  <!-- Connect to an instance -->
  <div class="row justify-content-center mt-5">
    <div class="col-md-6">
      <div class="card">
        <div class="card-body">
          <h5 class="card-title">Connect to Instance</h5>
          <p class="card-text text-body-secondary">
            Enter the URL of a Yesterdays instance to get started.
          </p>

          <form onsubmit={connect}>
            <div class="mb-3">
              <label for="instance-url" class="form-label">Instance URL</label>
              <input
                id="instance-url"
                type="url"
                class="form-control"
                placeholder="https://yesterdays.example.org"
                bind:value={auth.instanceUrl}
                required
                disabled={connectLoading}
              />
            </div>

            {#if connectError}
              <div class="alert alert-danger" role="alert">{connectError}</div>
            {/if}

            <div class="d-flex gap-2">
              <button type="submit" class="btn btn-primary" disabled={connectLoading || !auth.instanceUrl}>
                {#if connectLoading}
                  <span class="spinner-border spinner-border-sm me-1" role="status" aria-hidden="true"></span>
                  Connecting...
                {:else}
                  Connect
                {/if}
              </button>
              <button type="button" class="btn btn-outline-secondary" onclick={cancelConnect} disabled={connectLoading}>
                Back
              </button>
            </div>
          </form>
        </div>
      </div>
    </div>
  </div>

{:else}
  <!-- Landing (signed out): prepare, or connect -->
  <div class="text-center mt-5 mb-4">
    <h1 class="mb-2">Yesterdays Desktop Importer</h1>
    <p class="text-body-secondary mb-0">
      Prepare a folder of images offline, or connect to an instance to browse and import.
    </p>
  </div>

  <div class="row justify-content-center g-4 mb-4">
    <div class="col-sm-6 col-lg-5">
      <a
        href="/prepare-import"
        class="card h-100 text-reset text-decoration-none shadow-sm landing-card"
      >
        <div class="card-body text-center p-4 p-lg-5">
          <div class="landing-icon mb-3" aria-hidden="true">🗂️</div>
          <h2 class="h4 mb-2">Prepare an Image Folder</h2>
          <p class="text-body-secondary mb-0">
            Reorder, tag, and export a folder of images into an import-ready series with JSON sidecars.
          </p>
        </div>
      </a>
    </div>
    <div class="col-sm-6 col-lg-5">
      <button
        type="button"
        class="card h-100 w-100 text-reset shadow-sm landing-card"
        onclick={startConnect}
      >
        <div class="card-body text-center p-4 p-lg-5">
          <div class="landing-icon mb-3" aria-hidden="true">🔌</div>
          <h2 class="h4 mb-2">Connect to an Instance</h2>
          <p class="text-body-secondary mb-0">
            Sign in to a Yesterdays instance to browse its sources and import images.
          </p>
        </div>
      </button>
    </div>
  </div>
{/if}

<style>
  .landing-card {
    transition: border-color 0.1s ease;
  }
  .landing-card:hover {
    border-color: var(--bs-primary);
  }
  /* Let a <button> render identically to the anchor cards. */
  button.landing-card {
    padding: 0;
    font: inherit;
    text-align: inherit;
    cursor: pointer;
  }
  .landing-icon {
    font-size: 2.75rem;
    line-height: 1;
  }
</style>
