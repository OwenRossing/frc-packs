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
  /** How many of the sealed packs are boosted (they have their own stack). */
  boosted: number;
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
  /** Parts from scrapping extra copies, spent on boosted packs. */
  parts: number;
  boostCost: number;
  donateUrl: string | null;
  /** Parts for scrapping one extra copy, by tier. */
  scrapParts: Record<Tier, number>;
  demo: boolean;
  devTools: boolean;
  missions: Mission[];
  streak: Streak;
  wishlist: number[];
  showcase: number[];
  packs: PackState[];
  pending: Opening | null;
}

export interface Report {
  id: number;
  targetId: string;
  target: string;
  reporter: string;
  reason: "username" | "cheating" | "harassment" | "other";
  details: string;
  createdAt: number;
}

export interface Profile {
  username: string;
  me: boolean;
  joined: number;
  teams: number;
  total: number;
  cards: number;
  sets: number;
  trades: number;
  streak: number;
  /** Sealed standard packs and parts they have (what you can ask for in a trade). */
  packs: number;
  parts: number;
  showcase: CardRef[];
  rarest: CardRef[];
  wishlist: number[];
}

export interface Mission {
  id: string;
  label: string;
  goal: number;
  progress: number;
  /** Parts it pays. */
  reward: number;
  claimed: boolean;
}

export interface Streak {
  /** Days in a row with a pack opened (0 when the streak has lapsed). */
  days: number;
  /** A pack has been opened today. */
  today: boolean;
  alive: boolean;
  /** A boosted pack every this many days. */
  every: number;
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
  /** Parts a boosted pack costs. */
  boostCost: number;
  /** A donation page (https), or null for none. */
  donateUrl: string | null;
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

export interface Scrapped {
  gained: number;
  scrapped: number;
  /** Extra copies held back: in a pack still being revealed, or in an open trade offer. */
  held: number;
  state: State;
  collection: Collection;
}

export interface CardRef {
  num: number;
  tier: Tier;
  serial: number;
}

export interface Trade {
  id: number;
  /** True when you made the offer. */
  mine: boolean;
  /** The other player. */
  with: string;
  pack: string;
  youGive: CardRef[];
  youGet: CardRef[];
  youGivePacks: number;
  youGetPacks: number;
  youGiveParts: number;
  youGetParts: number;
  status: "open" | "accepted" | "declined" | "cancelled" | "failed";
  createdAt: number;
  decidedAt: number | null;
}

export interface Trades {
  incoming: Trade[];
  outgoing: Trade[];
  recent: Trade[];
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
  /** `boosted` picks the kind: true for a boosted pack, false for a standard one; left out, boosted goes first. */
  hand: (pack: string, boosted?: boolean) => call<{ best: Tier; boosted: boolean }>("POST", "/api/hand", { pack, boosted }),
  open: (pack: string, boosted?: boolean) => call<{ opening: Opening; state: State; streakReward: boolean }>("POST", "/api/open", { pack, boosted }),
  progress: (id: number, revealed: number) => call<void>("POST", `/api/openings/${id}/progress`, { revealed }),
  collection: (pack: string) => call<Collection>("GET", `/api/collection/${encodeURIComponent(pack)}`),
  /** Scrap extra copies of one team (the first copy is always kept). */
  scrap: (pack: string, num: number, count: number) => call<Scrapped>("POST", "/api/scrap", { pack, num, count }),
  /** Scrap every extra copy of these tiers. */
  scrapExtras: (pack: string, tiers: Tier[]) => call<Scrapped>("POST", "/api/scrap/extras", { pack, tiers }),
  /** Players whose username contains `q`, for picking who to trade with. */
  players: (q: string) => call<string[]>("GET", `/api/players?q=${encodeURIComponent(q)}`),
  playerCollection: (name: string, pack: string) =>
    call<Collection>("GET", `/api/players/${encodeURIComponent(name)}/collection/${encodeURIComponent(pack)}`),
  trades: () => call<Trades>("GET", "/api/trades"),
  /** Offer one copy each of `give` (your teams) for one copy each of `want` (theirs). */
  offerTrade: (to: string, pack: string, give: number[], want: number[], extra?: { givePacks: number; wantPacks: number; giveParts: number; wantParts: number }) =>
    call<Trades>("POST", "/api/trades", { to, pack, give, want, ...extra }),
  acceptTrade: (id: number) =>
    call<{ sets: string[]; state: State; collection: Collection; trades: Trades }>("POST", `/api/trades/${id}/accept`),
  declineTrade: (id: number) => call<Trades>("POST", `/api/trades/${id}/decline`),
  cancelTrade: (id: number) => call<Trades>("POST", `/api/trades/${id}/cancel`),
  /** The server's push key (null when push isn't set up), and turning notifications on or off for this browser. */
  pushKey: () => call<{ key: string | null }>("GET", "/api/push/key"),
  pushSubscribe: (sub: { endpoint: string; keys: { p256dh: string; auth: string } }) => call<void>("POST", "/api/push/subscribe", sub),
  pushUnsubscribe: (endpoint: string) => call<void>("POST", "/api/push/unsubscribe", { endpoint }),
  /** A player's profile: showcase, stats, rarest cards, wishlist. */
  profile: (name: string, pack: string) => call<Profile>("GET", `/api/players/${encodeURIComponent(name)}/profile/${encodeURIComponent(pack)}`),
  /** Put a team on your wishlist or take it off; returns the whole list. */
  setWish: (pack: string, team: number, on: boolean) => call<number[]>("POST", "/api/wishlist", { pack, team, on }),
  /** Pin up to 3 teams you own to your profile. */
  setShowcase: (pack: string, teams: number[]) => call<number[]>("POST", "/api/showcase", { pack, teams }),
  /** Report a player to the admin: reason is username, cheating, harassment or other. */
  report: (username: string, reason: string, details: string) => call<void>("POST", "/api/report", { username, reason, details }),
  /** Collect a finished daily mission's parts. */
  claimMission: (id: string) => call<State>("POST", `/api/missions/${encodeURIComponent(id)}/claim`),
  /** Spend parts on a boosted pack. */
  craft: () => call<State>("POST", "/api/craft"),
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
    rename: (id: string, username: string) => call<void>("POST", `/api/admin/users/${id}/rename`, { username }),
    reports: () => call<Report[]>("GET", "/api/admin/reports"),
    resolveReport: (id: number) => call<void>("POST", `/api/admin/reports/${id}/resolve`),
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
    not_enough_parts: "Not enough parts yet. Scrap some extra copies first.",
    no_extras: "No extra copies of that card to scrap.",
    extras_held: "Those copies are in a pack you're still opening or in a trade offer. Finish the pack or cancel the offer first.",
    unknown_player: "There's no player with that username.",
    not_owned: "One of those cards isn't available to trade any more.",
    trade_stale: "One of the cards in that trade isn't there any more, so the trade was called off.",
    trade_gone: "That trade was already answered or called off.",
    too_many_offers: "You have 20 offers waiting. Cancel some first.",
    bad_trade: "Pick 1 to 5 different cards on each side.",
    not_your_trade: "That trade isn't yours to answer.",
    not_enough_to_trade: "You don't have that many packs or parts to trade (boosted packs can't be traded).",
    they_lack: "They don't have that many packs or parts.",
    mission_not_ready: "That mission isn't finished yet.",
    wishlist_full: "Your wishlist is full (50). Take something off first.",
    username_not_allowed: "That username isn't allowed. Pick something else.",
    too_many_reports: "You've sent a lot of reports today. The admin will look at them.",
  };
  return words[e.code] ?? "Something went wrong. Try again.";
}

export type Api = typeof api;
