// Typed client for the Rust server's JSON API. The server decides everything that matters (what's in a pack,
// serial numbers, timers); this only asks and shows the answers.

export type Tier = "mythic" | "legendary" | "rare" | "uncommon" | "common";

export interface Team {
  num: number;
  name: string;
  epa: number | null;
  wl: string;
  div: string;
  srank: number;
  loc: string;
  rank: number;
  tier: Tier;
  photo: boolean;
}

export interface Odds {
  slots: Partial<Record<Tier, number>>;
  last: Partial<Record<Tier, number>>;
  mythicBase: number;
  demoMythic: number;
  soft: number;
  hard: number;
  legEvery: number;
}

export interface Recipe {
  id: string;
  name: string;
  where: string;
  season: number;
  event: string;
  claimable: boolean;
  odds: Odds;
  teams: Team[];
}

export interface Card {
  num: number;
  tier: Tier;
  serial: number;
  isNew: boolean;
  copy: number;
}

export interface Opening {
  id: number;
  pack: string;
  revealed: number;
  cards: Card[];
  sets: string[];
}

export interface PackState {
  id: string;
  sealed: number;
  opened: number;
  pity: { m: number; l: number };
}

export interface Account {
  username: string;
  admin: boolean;
}

export interface State {
  account: Account;
  /** Server time in ms, so timers don't depend on the device clock. */
  now: number;
  nextClaimAt: number;
  claimMs: number;
  /** Packs each timer gives. */
  claimPacks: number;
  /** How many missed timers wait to be claimed. */
  bank: number;
  demo: boolean;
  devTools: boolean;
  packs: PackState[];
  pending: Opening | null;
}

export interface Collection {
  pack: string;
  cards: { num: number; serials: number[] }[];
  sets: string[];
}

export interface Invite {
  /** Shown as ABCD-EFGH-JKMN. */
  code: string;
  note: string;
  maxUses: number;
  uses: number;
  createdAt: number;
  usedBy: string[];
}

/** The free-pack rules the admin sets. */
export interface Rules {
  /** Minutes between free packs. */
  claimMinutes: number;
  /** Packs each timer gives. */
  claimPacks: number;
  /** How many missed timers wait to be claimed. */
  bank: number;
  /** Packs a new account starts with. */
  startPacks: number;
}

export interface AdminUser {
  id: string;
  /** null for a guest account from before sign-in existed. */
  username: string | null;
  admin: boolean;
  disabled: boolean;
  createdAt: number;
  invite: string | null;
  inviteNote: string | null;
  sealed: number;
  opened: number;
  cards: number;
  lastOpenedAt: number | null;
}

/** status 0 means the server couldn't be reached. */
export class ApiError extends Error {
  constructor(public status: number, public code: string) {
    super(code);
  }
}

async function call<T>(method: "GET" | "POST", path: string, body?: unknown): Promise<T> {
  let res: Response;
  try {
    res = await fetch(path, {
      method,
      credentials: "same-origin",
      headers: method === "POST" ? { "Content-Type": "application/json" } : undefined,
      body: method === "POST" ? JSON.stringify(body ?? {}) : undefined,
    });
  } catch {
    throw new ApiError(0, "offline");
  }
  if (res.status === 204) return undefined as T;
  const data = await res.json().catch(() => null);
  if (!res.ok) throw new ApiError(res.status, (data && data.error) || "server_error");
  return data as T;
}

export const api = {
  /** The signed-in account's state. Fails with 401 `sign_in`, or `guest` for a guest account from before sign-in. */
  session: () => call<State>("POST", "/api/session"),
  signup: (code: string, username: string, password: string) =>
    call<State>("POST", "/api/signup", { code, username, password }),
  login: (username: string, password: string) => call<State>("POST", "/api/login", { username, password }),
  logout: () => call<void>("POST", "/api/logout"),
  changePassword: (current: string, next: string) => call<void>("POST", "/api/password", { current, new: next }),
  state: () => call<State>("GET", "/api/state"),
  claim: () => call<State>("POST", "/api/claim"),
  /** Pick up a pack: its cards are decided now; only the best rarity comes back, for the glow. */
  hand: (pack: string) => call<{ best: Tier }>("POST", "/api/hand", { pack }),
  open: (pack: string) => call<{ opening: Opening; state: State }>("POST", "/api/open", { pack }),
  progress: (id: number, revealed: number) => call<void>("POST", `/api/openings/${id}/progress`, { revealed }),
  collection: (pack: string) => call<Collection>("GET", `/api/collection/${encodeURIComponent(pack)}`),
  recipe: (pack: string) => call<Recipe>("GET", `/packs/${encodeURIComponent(pack)}.json`),
  dev: {
    demo: (on: boolean) => call<State>("POST", "/api/dev/demo", { on }),
    pack: (pack: string) => call<State>("POST", "/api/dev/pack", { pack }),
    skipTimer: () => call<State>("POST", "/api/dev/skip-timer"),
    reset: () => call<State>("POST", "/api/dev/reset"),
  },
  admin: {
    invites: () => call<Invite[]>("GET", "/api/admin/invites"),
    makeInvites: (note: string, uses: number, count: number) =>
      call<Invite[]>("POST", "/api/admin/invites", { note, uses, count }),
    deleteInvite: (code: string) => call<void>("POST", `/api/admin/invites/${encodeURIComponent(code)}/delete`),
    users: () => call<AdminUser[]>("GET", "/api/admin/users"),
    resetPassword: (id: string) => call<{ password: string }>("POST", `/api/admin/users/${id}/password`),
    givePacks: (id: string, count: number) => call<void>("POST", `/api/admin/users/${id}/packs`, { count }),
    setDisabled: (id: string, disabled: boolean) => call<void>("POST", `/api/admin/users/${id}/disabled`, { disabled }),
    deleteUser: (id: string, confirm: string) => call<void>("POST", `/api/admin/users/${id}/delete`, { confirm }),
    settings: () => call<Rules>("GET", "/api/admin/settings"),
    saveSettings: (r: Rules) => call<Rules>("POST", "/api/admin/settings", r),
  },
};

/** Friendly words for the server's error codes. */
export function explain(e: unknown): string {
  if (!(e instanceof ApiError)) return "Something went wrong. Try again.";
  const words: Record<string, string> = {
    offline: "Can't reach FRC Packs. Check your connection.",
    bad_login: "That username and password don't match.",
    disabled: "This account is turned off. Ask the person who invited you.",
    too_many_attempts: "Too many tries. Wait 15 minutes and try again.",
    bad_invite: "That invite code doesn't exist. Check it and try again.",
    invite_used: "That invite code has already been used.",
    username_taken: "That username is taken. Try another.",
    bad_username: "Usernames are 3 to 20 letters, numbers or underscores.",
    bad_password: "Passwords need at least 8 characters.",
    no_session: "You've been signed out. Reload to sign in again.",
    not_admin: "Only the admin account can do that.",
    not_yourself: "You can't do that to your own account.",
    confirm_mismatch: "The name you typed doesn't match.",
    not_found: "That no longer exists. Reload the page.",
  };
  return words[e.code] ?? "Something went wrong. Try again.";
}

export type Api = typeof api;
