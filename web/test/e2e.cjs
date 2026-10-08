// End-to-end checks of the real site: a browser bot plays packs against the Rust server.
// Needs the server running with DEV_TOOLS=1 and the built site (see the README):
//   BASE_URL=http://127.0.0.1:3000 npm test
const fs = require('fs');
const { chromium } = require('playwright');

const BASE = process.env.BASE_URL || 'http://127.0.0.1:3000/';
// Uses Playwright's own Chromium (npx playwright install chromium) unless CHROMIUM points at another one.
const CHROME = process.env.CHROMIUM || (fs.existsSync('/opt/pw-browsers/chromium') ? '/opt/pw-browsers/chromium' : undefined);
let fails = 0;
const check = (ok, msg) => { console.log((ok ? 'ok   ' : 'FAIL ') + msg); if (!ok) fails++; };

const st = p => p.evaluate(() => window.__frc.state());
const waitState = (p, s, timeout = 6000) => p.waitForFunction(s => window.__frc.state() === s, s, { timeout });
const ready = p => p.waitForFunction(() => document.body.classList.contains('ready'), null, { timeout: 8000 });
const packs = async p => +(await p.locator('#packCount').textContent());
async function fresh(b, viewport = { width: 390, height: 844 }) {
  const c = await b.newContext({ viewport });
  const p = await c.newPage(); p.errs = [];
  p.on('pageerror', e => p.errs.push(String(e)));
  p.on('console', m => { if (m.type() === 'error' && !/Failed to load resource/.test(m.text())) p.errs.push(m.text()); });
  await p.goto(BASE); await ready(p);
  return { c, p };
}
async function dev(p, path, body = {}) {
  return p.evaluate(async ([path, body]) => (await fetch('/api/dev/' + path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) })).status, [path, body]);
}
async function toRing(p) { if ((await st(p)) === 'home') { await p.locator('.ptype').first().click(); await waitState(p, 'select'); } }
async function pickPack(p) { await toRing(p); await p.waitForTimeout(150); await p.locator('.pick.front').click({ force: true }); await waitState(p, 'inspect'); }
async function openOne(p, { swipe = false, deal = 99 } = {}) {
  if (['home', 'select'].includes(await st(p))) await pickPack(p);
  if (swipe) {
    await p.waitForTimeout(550);
    const b = await p.locator('#inspect').boundingBox();
    await p.mouse.move(b.x + b.width * .06, b.y + b.height * .12); await p.mouse.down();
    await p.mouse.move(b.x + b.width * .5, b.y + b.height * .12, { steps: 6 });
    await p.mouse.move(b.x + b.width * .96, b.y + b.height * .12, { steps: 6 });
    await p.mouse.up();
  } else await p.click('#openBtn');
  await waitState(p, 'stack');
  for (let n = 0; n < 40 && deal > 0 && (await st(p)) !== 'summary'; n++) {
    if ((await st(p)) === 'stack') {
      const hidden = await p.evaluate(() => document.querySelector('#stack').lastElementChild._hidden);
      await p.click('#stack', { force: true });
      if (!hidden) deal--;
    }
    await p.waitForTimeout(220);
  }
  if (deal > 0) await waitState(p, 'summary');
}
async function noScroll(p, label) {
  const o = await p.evaluate(() => document.documentElement.scrollWidth - innerWidth);
  check(o <= 0, `no horizontal scroll: ${label} (${o})`);
}

(async () => {
  const b = await chromium.launch(CHROME ? { executablePath: CHROME } : {});

  // 1. A first visit makes a guest account with 2 packs and a free pack to claim.
  const { c, p } = await fresh(b);
  check(await packs(p) === 2, 'new account starts with 2 packs');
  check(await p.locator('#claim').isVisible(), 'a free pack is ready to claim');
  await p.click('#claim'); await p.click('#claim', { force: true }).catch(() => {}); await p.waitForTimeout(500);
  check(await packs(p) === 3, 'claiming adds one pack, and a double click adds nothing');
  check(!(await p.locator('#claim').isVisible()), 'claim button hides until the timer runs out');
  check(/Next free pack in <b>(5:00:00|4:5\d:\d\d)<\/b>/.test(await p.locator('#timer').innerHTML()), 'timer shows the server\'s 5 hour wait');

  // 2. The pack in hand is decided by the server and doesn't change if you put it back.
  await pickPack(p); await p.waitForTimeout(400);
  const tell1 = await p.evaluate(() => document.querySelector('.inspect').style.getPropertyValue('--tell'));
  await p.click('#backBtn'); await waitState(p, 'select');
  await pickPack(p); await p.waitForTimeout(400);
  const tell2 = await p.evaluate(() => document.querySelector('.inspect').style.getPropertyValue('--tell'));
  check(tell1 && tell1 === tell2, `putting the pack back keeps the same glow (${tell1})`);
  check(await p.evaluate(() => getComputedStyle(document.querySelector('.inspect .tell')).opacity) === '0', 'no glow before swiping');

  // 3. First pack: swipe open, best card last and Legendary or better.
  await openOne(p, { swipe: true });
  const labels = await p.locator('#summary .slot').evaluateAll(els => els.map(e => e.getAttribute('aria-label')));
  check(labels.length === 5, 'summary shows 5 cards');
  check(/Legendary|Mythic/.test(labels[4]), 'first pack ends in Legendary or better');
  const tierColor = { Legendary: '#b478ff', Mythic: '#b478ff', Rare: '#ffc850', Uncommon: '#7fe0d2', Common: '#aab8c4' };
  check(tierColor[labels[4].split(', ').pop()] === tell1, 'the glow matched the best card');
  check(await packs(p) === 2, 'opening uses a pack');
  check(await p.locator('#meterTxt').textContent() === '1 / 200', 'Mythic meter counts the pack');
  check(/^No\. \d+$/.test((await p.locator('#summary .slot .ft span').nth(1).textContent()).trim()), 'cards carry server serial numbers');
  await noScroll(p, 'summary');

  // 4. "Open another pack" brings the wheel back.
  await p.click('#again'); await waitState(p, 'select');
  check(await p.locator('.pick').count() === 10, 'open another pack shows the wheel of 10');
  await noScroll(p, 'wheel');

  // 5. Close the page mid-reveal: the pack picks up where it left off.
  await openOne(p, { deal: 2 });
  const counter = await p.locator('#counter i.done').count();
  await p.reload(); await ready(p);
  check(await st(p) === 'stack', 'reloading mid-reveal goes back to the stack');
  check(await p.locator('#counter i.done').count() === counter && counter === 2, `it resumes after the ${counter} cards already dealt`);
  check(await packs(p) === 1, 'resuming does not use another pack');
  await p.click('#skipBtn'); await waitState(p, 'summary');
  check(await p.locator('#summary .slot').count() === 5, 'reveal all finishes the resumed pack');
  await p.reload(); await ready(p);
  check(await st(p) === 'home', 'a finished pack does not resume again');

  // 6. The collection lives on the server.
  await p.click('#tabBinder'); await p.waitForTimeout(300);
  const sum = await p.locator('#binderSum').textContent();
  check(/· 10 cards$/.test(sum), `binder counts all 10 cards (${sum})`);
  await noScroll(p, 'binder');
  const other = await fresh(b);
  await other.p.click('#tabBinder'); await other.p.waitForTimeout(300);
  check(/^0 of \d+ teams · 0 cards$/.test(await other.p.locator('#binderSum').textContent()), 'another browser gets its own empty account');
  await other.c.close();

  // 7. Two tabs: a pack opened in one shows up in the other when you come back to it.
  await p.click('#tabOpen');
  const p2 = await c.newPage(); await p2.goto(BASE); await ready(p2);
  await openOne(p2);
  check(await packs(p2) === 0, 'second tab opened the last pack');
  await p.bringToFront(); await p.evaluate(() => window.dispatchEvent(new Event('focus'))); await p.waitForTimeout(600);
  check(await packs(p) === 0, 'first tab catches up when focused');
  await p2.close();

  // 8. Out of packs.
  await p.locator('.ptype').first().click(); await p.waitForTimeout(300);
  check(await st(p) === 'home', 'an empty pack tile does not open the wheel');
  check(await p.locator('.ptype.empty').count() === 1, 'the tile shows it is empty');

  // 9. Losing the connection mid-open keeps the pack in your hand.
  await dev(p, 'pack', { pack: 'cmp26' }); await p.reload(); await ready(p);
  await pickPack(p);
  await p.route('**/api/open', r => r.abort());
  await p.click('#openBtn'); await p.waitForTimeout(500);
  check(await st(p) === 'inspect', 'a failed open leaves the pack in hand');
  check(/Can't reach the server/.test(await p.locator('#banner').textContent()), 'and says the server could not be reached');
  await p.unroute('**/api/open');
  await p.click('#openBtn'); await waitState(p, 'stack');
  check(await packs(p) === 0, 'trying again opens it');
  await p.click('#skipBtn'); await waitState(p, 'summary');

  // 10. Testing helpers (only when the server runs with DEV_TOOLS=1).
  await p.click('#gear');
  check(await p.locator('#demoLuck').isVisible(), 'dev tools show in settings');
  await p.click('#demoPack'); await p.waitForTimeout(400);
  check(await packs(p) === 1, '+1 demo pack works');
  await p.click('#reset'); await p.click('#reset'); await p.waitForTimeout(500);
  check(await packs(p) === 2, 'reset starts over with 2 packs');
  await p.click('#gear');
  await p.click('#tabBinder'); await p.waitForTimeout(300);
  check(/· 0 cards$/.test(await p.locator('#binderSum').textContent()), 'reset empties the binder');
  check(p.errs.length === 0, 'no errors in the page: ' + JSON.stringify(p.errs));
  await c.close();

  // 11. Layouts.
  for (const w of [320, 768, 1440]) {
    const { c, p } = await fresh(b, { width: w, height: w > 800 ? 900 : 760 });
    await noScroll(p, `home at ${w}px`);
    await toRing(p); await p.waitForTimeout(1200); await noScroll(p, `wheel at ${w}px`);
    await openOne(p); await noScroll(p, `summary at ${w}px`);
    check(p.errs.length === 0, `no errors at ${w}px`);
    await c.close();
  }

  await b.close();
  console.log(fails ? `\n${fails} FAILED` : '\nALL PASSED');
  process.exit(fails ? 1 : 0);
})().catch(e => { console.error(e); process.exit(1); });
