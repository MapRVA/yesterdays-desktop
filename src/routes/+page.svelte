<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { goto } from "$app/navigation";
  import { auth, type User } from "$lib/auth.svelte";

  let connectError = $state("");
  let connectLoading = $state(false);

  let authError = $state("");
  let authLoading = $state(false);
  let cancelRequested = $state(false);

  $effect(() => {
    if (auth.hydrated && auth.user) {
      goto("/browse", { replaceState: true });
    }
  });

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
</script>

<div class="row justify-content-center mt-5">
  <div class="col-md-6">
    {#if !auth.connected}
      <!-- Step 1: Connect to instance -->
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

            <button type="submit" class="btn btn-primary" disabled={connectLoading || !auth.instanceUrl}>
              {#if connectLoading}
                <span class="spinner-border spinner-border-sm me-1" role="status" aria-hidden="true"></span>
                Connecting...
              {:else}
                Connect
              {/if}
            </button>
          </form>
        </div>
      </div>

    {:else if !auth.user}
      <!-- Step 2: Log in -->
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
              <button class="btn btn-outline-secondary" onclick={() => auth.disconnect()}>
                Back
              </button>
            {/if}
          </div>
        </div>
      </div>

    {/if}
  </div>
</div>
