// Installing FRC Packs as an app, and push notifications. Both are rows in Settings.
//
// - Install: Chrome and Edge (Android, Windows, Mac, ChromeOS) offer an install prompt the page can trigger. iPhone
//   and iPad Safari don't; there the row explains Share → Add to Home Screen.
// - Notifications: free pack ready, trade offers, trades accepted. On iPhone they only work once FRC Packs is added
//   to the home screen (Apple's rule), so the row says that first.

import { api } from "./api";

type InstallPrompt = Event & { prompt: () => Promise<void>; userChoice: Promise<{ outcome: string }> };

const $ = <T extends HTMLElement = HTMLElement>(s: string) => document.querySelector<T>(s)!;
const ios = /iPad|iPhone|iPod/.test(navigator.userAgent) || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
const standalone = () => matchMedia("(display-mode: standalone)").matches || (navigator as unknown as { standalone?: boolean }).standalone === true;
const canPush = () => "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;

let reg: ServiceWorkerRegistration | null = null;
let installPrompt: InstallPrompt | null = null;

function b64(buf: ArrayBuffer | null): string {
  if (!buf) return "";
  return btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
function fromB64(s: string): Uint8Array<ArrayBuffer> {
  const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (s.length % 4)) % 4));
  const out = new Uint8Array(new ArrayBuffer(bin.length));
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

async function currentSub(): Promise<PushSubscription | null> {
  return reg ? reg.pushManager.getSubscription() : null;
}

async function paintPush() {
  const btn = $<HTMLButtonElement>("#pushBtn"), txt = $("#pushTxt");
  btn.disabled = false;
  if (ios && !standalone()) {
    txt.textContent = "On iPhone and iPad, add FRC Packs to your home screen first (Share, then Add to Home Screen), then turn them on from there.";
    btn.textContent = "Off"; btn.disabled = true; return;
  }
  if (!canPush() || !reg) { txt.textContent = "This browser can't show notifications."; btn.textContent = "Off"; btn.disabled = true; return; }
  if (Notification.permission === "denied") {
    txt.textContent = "Notifications are blocked for this site. Allow them in your browser's site settings, then come back.";
    btn.textContent = "Blocked"; btn.disabled = true; return;
  }
  const on = !!(await currentSub()) && Notification.permission === "granted";
  btn.textContent = on ? "On" : "Off"; btn.setAttribute("aria-pressed", String(on));
  txt.textContent = "Free pack ready, trade offers and accepted trades, on this device.";
}

async function turnOn() {
  const { key } = await api.pushKey();
  if (!key || !reg) throw new Error("push unavailable");
  if ((await Notification.requestPermission()) !== "granted") return;
  const sub = (await currentSub()) ?? (await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: fromB64(key) }));
  await api.pushSubscribe({ endpoint: sub.endpoint, keys: { p256dh: b64(sub.getKey("p256dh")), auth: b64(sub.getKey("auth")) } });
}

async function turnOff() {
  const sub = await currentSub();
  if (!sub) return;
  await api.pushUnsubscribe(sub.endpoint).catch(() => {});
  await sub.unsubscribe();
}

function paintInstall() {
  const line = $("#installLine"), btn = $<HTMLButtonElement>("#installBtn"), txt = $("#installTxt");
  if (standalone()) { line.hidden = true; return; }
  line.hidden = false;
  if (installPrompt) { txt.textContent = "Put FRC Packs on your home screen or desktop, in its own window."; btn.hidden = false; return; }
  btn.hidden = true;
  txt.textContent = ios
    ? "In Safari, tap Share, then Add to Home Screen."
    : "Use your browser's menu (Install app, or Add to Home screen).";
}

export function setupPwa() {
  if ("serviceWorker" in navigator) {
    navigator.serviceWorker.register("/sw.js").then((r) => { reg = r; paintPush(); }, () => paintPush());
  } else paintPush();
  addEventListener("beforeinstallprompt", (e) => { e.preventDefault(); installPrompt = e as InstallPrompt; paintInstall(); });
  addEventListener("appinstalled", () => { installPrompt = null; paintInstall(); });
  paintInstall();

  $("#installBtn").onclick = async () => {
    if (!installPrompt) return;
    await installPrompt.prompt();
    await installPrompt.userChoice.catch(() => null);
    installPrompt = null; paintInstall();
  };
  $("#pushBtn").onclick = async () => {
    const btn = $<HTMLButtonElement>("#pushBtn");
    btn.disabled = true;
    try {
      if (await currentSub()) await turnOff(); else await turnOn();
      await paintPush();
    } catch (e) {
      console.warn("notifications:", e);
      await paintPush();
      $("#pushTxt").textContent = "Couldn't change notifications. Try again.";
    }
  };
}
