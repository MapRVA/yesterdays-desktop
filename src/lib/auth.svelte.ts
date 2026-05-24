import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { load, type Store } from "@tauri-apps/plugin-store";

export type User = {
  osm_id: number;
  username: string;
  is_staff: boolean;
  can_import: boolean;
  scopes: string[];
};

type StoredSession = {
  instanceUrl: string;
  instanceName: string;
  accessToken: string;
  refreshToken: string | null;
  expiresAt: number;
  user: User;
};

type TokenResponse = {
  access_token: string;
  refresh_token: string | null;
  expires_in: number;
  token_type: string;
  scope: string;
};

const STORE_FILE = "auth.json";
const SESSION_KEY = "session";
const REFRESH_SAFETY_WINDOW_MS = 60_000;

class AuthStore {
  instanceUrl = $state("");
  instanceName = $state("");
  connected = $state(false);
  user: User | null = $state(null);
  accessToken: string | null = $state(null);
  refreshToken: string | null = $state(null);
  expiresAt: number = $state(0);
  hydrated = $state(false);

  #store: Store | null = null;

  async #getStore(): Promise<Store> {
    if (!this.#store) {
      this.#store = await load(STORE_FILE, { autoSave: true, defaults: {} });
    }
    return this.#store;
  }

  async hydrate() {
    if (this.hydrated) return;
    // Listen for background refreshes (e.g. from the replacements dispatcher
    // when it catches a 401 mid-batch) so the stored session stays in sync.
    void listen<TokenResponse>("tokens-refreshed", (ev) => {
      void this.applyRefreshedTokens(ev.payload);
    });
    try {
      const store = await this.#getStore();
      const saved = await store.get<StoredSession>(SESSION_KEY);
      if (!saved) return;

      this.instanceUrl = saved.instanceUrl;
      this.instanceName = saved.instanceName;
      this.connected = true;

      const now = Date.now();
      if (saved.expiresAt > now + REFRESH_SAFETY_WINDOW_MS) {
        this.accessToken = saved.accessToken;
        this.refreshToken = saved.refreshToken;
        this.expiresAt = saved.expiresAt;
        this.user = saved.user;
        return;
      }

      if (saved.refreshToken) {
        try {
          const tokens = await invoke<TokenResponse>("refresh_access_token", {
            instanceUrl: saved.instanceUrl,
            refreshToken: saved.refreshToken,
          });
          this.accessToken = tokens.access_token;
          this.refreshToken = tokens.refresh_token ?? saved.refreshToken;
          this.expiresAt = Date.now() + tokens.expires_in * 1000;
          this.user = saved.user;
          await this.#persist();
          return;
        } catch {
          // fall through to clearing session below
        }
      }

      await this.#clear();
    } finally {
      this.hydrated = true;
    }
  }

  async setSession(params: {
    user: User;
    accessToken: string;
    refreshToken: string | null;
    expiresIn: number;
  }) {
    this.user = params.user;
    this.accessToken = params.accessToken;
    this.refreshToken = params.refreshToken;
    this.expiresAt = Date.now() + params.expiresIn * 1000;
    await this.#persist();
  }

  async applyRefreshedTokens(tokens: TokenResponse) {
    this.accessToken = tokens.access_token;
    if (tokens.refresh_token) this.refreshToken = tokens.refresh_token;
    this.expiresAt = Date.now() + tokens.expires_in * 1000;
    await this.#persist();
  }

  async #persist() {
    if (!this.user || !this.accessToken) return;
    const session: StoredSession = {
      instanceUrl: this.instanceUrl,
      instanceName: this.instanceName,
      accessToken: this.accessToken,
      refreshToken: this.refreshToken,
      expiresAt: this.expiresAt,
      user: this.user,
    };
    const store = await this.#getStore();
    await store.set(SESSION_KEY, session);
  }

  async #clear() {
    this.user = null;
    this.accessToken = null;
    this.refreshToken = null;
    this.expiresAt = 0;
    const store = await this.#getStore();
    await store.delete(SESSION_KEY);
  }

  async disconnect() {
    this.connected = false;
    this.instanceName = "";
    await this.#clear();
    try {
      await invoke("clear_all_collection_caches");
    } catch (e) {
      console.error("clear caches on logout:", e);
    }
  }
}

export const auth = new AuthStore();
