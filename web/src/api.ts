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

export interface State {
  /** Server time in ms, so timers don't depend on the device clock. */
  now: number;
  nextClaimAt: number;
  claimMs: number;
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
  /** Current account, creating a guest account on first visit. */
  session: () => call<State>("POST", "/api/session"),
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
};

export type Api = typeof api;
