// The account rows in Settings: who you're signed in as, signing out, changing your password, and the admin panel link.

import { api, explain, type Account } from "./api";

export function accountSettings(account: Account) {
  const $ = <T extends HTMLElement = HTMLElement>(s: string) => document.querySelector<T>(s)!;
  $("#acctTxt").textContent = `Signed in as ${account.username}.`;
  $("#adminLine").hidden = !account.admin;

  $("#signOut").onclick = async () => {
    try {
      await api.logout();
    } finally {
      location.assign("/");
    }
  };

  const btn = $("#pwBtn"), form = $<HTMLFormElement>("#pwForm"), msg = $("#pwMsg");
  const current = form.elements.namedItem("current") as HTMLInputElement;
  const next = form.elements.namedItem("next") as HTMLInputElement;
  btn.onclick = () => {
    form.hidden = !form.hidden;
    btn.setAttribute("aria-expanded", String(!form.hidden));
    msg.hidden = true;
    if (!form.hidden) current.focus();
  };
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    const say = (text: string, ok = false) => {
      msg.textContent = text;
      msg.classList.toggle("ok", ok);
      msg.hidden = false;
    };
    if (!current.value) return say("Enter your current password.");
    if (next.value.length < 8) return say("New passwords need at least 8 characters.");
    try {
      await api.changePassword(current.value, next.value);
      current.value = next.value = "";
      say("Password changed.", true);
    } catch (err) {
      say(explain(err));
    }
  });
}
