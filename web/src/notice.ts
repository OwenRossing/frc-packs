/* A notice from the site owner: a red bar across the top of every screen with a countdown, and a full-screen message that
   has to be acknowledged. It reads /notice.json ({ "text": "...", "at": <shutdown time in ms> }) and goes away when the
   file does. */
type Notice = { text: string; at: number };

let notice: Notice | null = null;
let bar: HTMLElement | null = null;
let overlay: HTMLElement | null = null;
let shownFor = 0;

function left(ms: number): string {
  if (ms <= 0) return "any moment now";
  const s = Math.floor(ms / 1000), h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60);
  return h > 0 ? `${h}h ${m}m` : m > 0 ? `${m}m ${s % 60}s` : `${s}s`;
}

function when(at: number): string {
  return new Date(at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

function paint() {
  if (!notice) return;
  const ms = notice.at - Date.now();
  if (!bar) {
    bar = document.createElement("div");
    bar.className = "notice-bar";
    bar.setAttribute("role", "alert");
    document.body.prepend(bar);
    document.body.classList.add("has-notice");
  }
  bar.textContent = `${notice.text} Offline in ${left(ms)} (${when(notice.at)}).`;
  // The full-screen message shows once when the page opens, and again at 10 minutes and 1 minute to go.
  const stage = ms <= 60_000 ? 3 : ms <= 600_000 ? 2 : 1;
  if (stage > shownFor && !overlay) {
    shownFor = stage;
    overlay = document.createElement("div");
    overlay.className = "notice-overlay";
    overlay.setAttribute("role", "alertdialog");
    overlay.setAttribute("aria-modal", "true");
    const box = document.createElement("div");
    box.className = "notice-box";
    const h = document.createElement("h2");
    h.textContent = "Heads up";
    const p = document.createElement("p");
    p.textContent = notice.text;
    const t = document.createElement("strong");
    t.className = "notice-time";
    const b = document.createElement("button");
    b.type = "button";
    b.className = "cta";
    b.textContent = "Got it";
    b.onclick = () => {
      overlay?.remove();
      overlay = null;
    };
    box.append(h, p, t, b);
    overlay.append(box);
    document.body.append(overlay);
  }
  const t = overlay?.querySelector(".notice-time");
  if (t) t.textContent = `Offline in ${left(ms)} (${when(notice.at)})`;
}

async function check() {
  try {
    const r = await fetch(`/notice.json?t=${Date.now()}`, { cache: "no-store" });
    const j = r.ok && (r.headers.get("content-type") || "").includes("json") ? await r.json() : null;
    notice = j && typeof j.text === "string" && typeof j.at === "number" ? { text: j.text, at: j.at } : null;
  } catch {
    return;
  }
  if (!notice) {
    bar?.remove();
    overlay?.remove();
    bar = overlay = null;
    shownFor = 0;
    document.body.classList.remove("has-notice");
  } else paint();
}

export function startNotice() {
  void check();
  setInterval(() => void check(), 30_000);
  setInterval(paint, 1000);
}
