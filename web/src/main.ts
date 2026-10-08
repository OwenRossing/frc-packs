import "./style.css";
import { api, ApiError } from "./api";
import { start } from "./app.js";

const PACK = "cmp26";

async function main() {
  try {
    const recipe = await api.recipe(PACK);
    start(recipe);
  } catch (e) {
    const offline = e instanceof ApiError && e.status === 0;
    document.body.insertAdjacentHTML(
      "afterbegin",
      `<div class="fatal">${offline ? "Can't reach FRC Packs right now. Check your connection and reload." : "Something went wrong loading FRC Packs. Reload to try again."}</div>`,
    );
    throw e;
  }
}

main();
