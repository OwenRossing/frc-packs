// Play-test bot for the FRC Packs prototype. Run: NODE_PATH=$(npm root -g) node test/play.js
const { chromium } = require('playwright');
const path = require('path');
const { url, out, openOne, waitState } = require('./lib');
require('fs').mkdirSync(out, { recursive: true });
const fails = [];
const check = (ok, msg) => { if (!ok) { fails.push(msg); console.log('FAIL', msg); } else console.log('ok  ', msg); };

(async () => {
  const b = await chromium.launch({ executablePath: '/opt/pw-browsers/chromium' });
  const ctx = await b.newContext({ viewport: { width: 400, height: 800 }, hasTouch: false });
  const p = await ctx.newPage();
  const errs = []; p.on('pageerror', e => errs.push(String(e))); p.on('console', m => { if (m.type() === 'error' && !/fonts\.g|ERR_|Failed to load resource/.test(m.text())) errs.push(m.text()); });
  await p.goto(url);

  // 1. data and odds
  const sim = await p.evaluate(() => {
    const f = window.__frc, tiers = {}, last = {}; let bad = 0, dupes = 0, myth = 0; const N = 200000;
    for (let i = 0; i < N; i++) {
      const c = f.rollPack(false); if (c.length !== 5) bad++;
      const seen = {}; c.forEach((x, k) => { tiers[x.tier] = (tiers[x.tier] || 0) + 1; if (seen[x.num]) dupes++; seen[x.num] = 1; if (f.TEAMS.find ? false : false) {} });
      const l = c[4].tier; last[l] = (last[l] || 0) + 1; if (l === 'mythic') myth++;
      if (!['rare', 'legendary', 'mythic'].includes(l)) bad++;
    }
    const pools = {}; Object.keys(f.POOL).forEach(k => pools[k] = f.POOL[k].length);
    return { N, tiers, last, bad, dupes, myth, pools, teams: f.TEAMS.length };
  });
  console.log(JSON.stringify(sim));
  check(sim.bad === 0, 'every pack has 5 cards and a Rare+ last slot');
  check(sim.dupes === 0, 'no duplicate team inside one pack');
  const slotShare = t => sim.tiers[t] / (sim.N * 4 + sim.N * 1);
  check(Math.abs(sim.tiers.common / (sim.N * 4) - .70) < .01, 'common about 70% of slots 1-4 (' + (sim.tiers.common / (sim.N * 4)).toFixed(3) + ')');
  check(sim.myth >= 1 && sim.myth <= 30, 'mythic about 1 in 20,000 packs (' + sim.myth + ' in ' + sim.N + ')');
  check(Object.values(sim.pools).every(n => n > 20), 'every tier has cards in the pool ' + JSON.stringify(sim.pools));

  // 2. fresh load state
  check(await p.locator('#packCount').textContent() === '2', 'starts with 2 packs');
  check(await p.locator('#claim').isVisible(), 'free pack is claimable on first visit');
  await p.screenshot({ path: out + '/01-start.png' });
  await p.click('#claim'); await p.waitForTimeout(200);
  check(await p.locator('#packCount').textContent() === '3', 'claim adds a pack');
  check(!(await p.locator('#claim').isVisible()), 'claim button hides after claiming');
  await p.click('#claim', { force: true }).catch(() => {});
  check(await p.locator('#packCount').textContent() === '3', 'double claim does nothing');

  // 3. play three packs through the UI (button, swipe-cut + swipe cards, button), demo luck on
  check((await p.locator('.pick').count()) === 10, 'shelf shows 10 packs');
  await p.screenshot({ path: out + '/01b-shelf.png' });
  await p.locator('.pick').nth(5).click({ force: true }); await waitState(p, 'inspect'); await p.waitForTimeout(600);
  await p.screenshot({ path: out + '/01c-inspect.png' });
  { const b = await p.locator('#inspect').boundingBox();
    await p.mouse.move(b.x + b.width / 2, b.y + b.height * .6); await p.mouse.down();
    await p.mouse.move(b.x + b.width / 2 + 280, b.y + b.height * .6, { steps: 10 }); await p.mouse.up(); await p.waitForTimeout(600);
    check(await p.evaluate(() => document.querySelector('#inspect').classList.contains('away')), 'dragging the pack turns it to the back');
    await p.screenshot({ path: out + '/01d-pack-back.png' });
    check((await p.locator('#packCount').textContent()) === '3', 'turning the pack over spends nothing');
    await p.click('#backBtn'); await waitState(p, 'select'); }
  await openOne(p); await p.waitForTimeout(700); await p.screenshot({ path: out + '/02-pack1-done.png' });
  check((await p.locator('#summary .slot').count()) === 5, 'summary shows all 5 cards');
  check(await p.locator('#packCount').textContent() === '2', 'opening a pack uses one');
  await p.click('#again'); await waitState(p, 'inspect');
  check(true, 'open another pack goes straight to a pack in hand');
  await p.click('#backBtn'); await waitState(p, 'select');
  await openOne(p, { swipe: true, swipeCards: true });
  await p.click('#again'); await waitState(p, 'inspect');
  // reveal all: skip the rest of the stack and land on the summary with every card
  await p.click('#openBtn'); await waitState(p, 'stack');
  check(await p.locator('#skipBtn').isVisible(), 'reveal all is offered during the stack');
  await p.click('#skipBtn'); await waitState(p, 'summary');
  check((await p.locator('#summary .slot').count()) === 5 && !(await p.evaluate(() => window.__frc.S().pending)), 'reveal all lands on the summary and finishes the pack');
  check(!(await p.locator('#skipBtn').isVisible()), 'reveal all hides on the summary');
  check(/of 515 collected/.test(await p.locator('#sub').textContent()), 'summary shows collection progress');
  const dotN = await p.evaluate(() => Object.keys(window.__frc.S().unseen).length);
  check(dotN > 0 && (await p.locator('#tabDot').textContent()) === String(dotN), 'binder tab counts new teams (' + dotN + ')');
  check(await p.locator('#again').textContent() === 'Back to the packs', 'last pack offers back to the packs');
  await p.click('#again'); await p.waitForTimeout(300);
  check((await p.locator('#hint').textContent()) === 'Out of packs', 'shelf says out of packs');
  await p.locator('.pick').nth(5).click({ force: true }); await p.waitForTimeout(200);
  check((await p.evaluate(() => window.__frc.state())) === 'select', 'an empty shelf will not open');
  await p.screenshot({ path: out + '/03-empty.png' });
  check((await p.evaluate(() => window.__frc.S().opened)) === 3, 'opened counter is 3');

  // 4. persistence + binder
  await p.reload(); await p.waitForTimeout(300);
  const inv = await p.evaluate(() => Object.values(window.__frc.S().inv).reduce((a, b) => a + b.length, 0));
  check(inv === 15, 'collection survives reload (15 cards): ' + inv);
  await p.click('#tabBinder'); await p.waitForTimeout(300);
  await p.screenshot({ path: out + '/04-binder.png' });
  check((await p.locator('#grid .ribbon:visible').count()) > 0, 'binder marks newly pulled teams');
  const cells = await p.locator('#grid .slot').count();
  check(cells > 0 && cells <= 15, 'binder shows owned cards: ' + cells);
  await p.locator('#grid .slot').first().click(); await p.waitForTimeout(300);
  check(await p.locator('#modal').isVisible(), 'inspect modal opens');
  await p.screenshot({ path: out + '/05-inspect.png' });
  await p.keyboard.press('Escape'); await p.waitForTimeout(100);
  check(!(await p.locator('#modal').isVisible()), 'escape closes the modal');
  await p.click('.tiers .chip:nth-child(2)'); await p.waitForTimeout(100);

  // 5. resume mid-reveal after reload
  await p.click('#tabOpen');
  check((await p.evaluate(() => Object.keys(window.__frc.S().unseen).length)) === 0 && !(await p.locator('#tabDot').isVisible()), 'leaving the binder clears the new marks'); await p.click('#gear'); await p.click('#demoPack'); await p.click('#gear');
  await p.locator('.pick').nth(5).click({ force: true }); await waitState(p, 'inspect'); await p.click('#openBtn'); await waitState(p, 'stack');
  await p.click('#stack', { force: true }); await p.waitForTimeout(600); await waitState(p, 'stack');
  const before = await p.evaluate(() => JSON.stringify(window.__frc.S().pending.cards.map(c => c.num)));
  await p.reload(); await p.waitForTimeout(500);
  const after = await p.evaluate(() => { const s = window.__frc.S(); return { st: window.__frc.state(), cards: s.pending && JSON.stringify(s.pending.cards.map(c => c.num)), rev: s.pending && s.pending.revealed, left: document.querySelectorAll('#stack .slot').length }; });
  check(after.cards === before && after.rev === 1 && after.left === 4, 'closing mid-pack resumes the same cards at card 2 ' + JSON.stringify(after));
  check(after.st === 'stack', 'resumed pack shows the stack');
  await p.screenshot({ path: out + '/06-resume.png' });

  // 6. corrupt storage, blocked storage
  await p.evaluate(() => localStorage.setItem('frcpacks.cmp26', '{not json'));
  await p.reload(); await p.waitForTimeout(300);
  check(await p.locator('#packCount').textContent() === '2', 'corrupt save falls back to a fresh start');
  await p.evaluate(() => localStorage.setItem('frcpacks.cmp26', JSON.stringify({ v: 1, packs: 1, nextClaimAt: Date.now() + 99 * 3600e3, inv: {}, pending: { cards: [{ num: 99999999, tier: 'rare' }], revealed: 0 }, muted: false, demo: true })));
  await p.reload(); await p.waitForTimeout(300);
  check((await p.evaluate(() => window.__frc.S().pending)) === null, 'pending with unknown team is dropped');
  check((await p.evaluate(() => window.__frc.S().nextClaimAt - Date.now())) <= 5 * 3600e3 + 2000, 'clock set far ahead is clamped to 5h');

  check(errs.length === 0, 'no console or page errors ' + JSON.stringify(errs));
  await ctx.close();

  // 7. blocked storage context
  const c2 = await b.newContext({ viewport: { width: 400, height: 800 } });
  await c2.addInitScript(() => { Object.defineProperty(window, 'localStorage', { get() { throw new Error('blocked'); } }); });
  const p2 = await c2.newPage(); const e2 = []; p2.on('pageerror', e => e2.push(String(e)));
  await p2.goto(url); await p2.waitForTimeout(300);
  check(await p2.locator('#packCount').textContent() === '2', 'works with storage blocked');
  await p2.click('#gear'); check(await p2.locator('#storageWarn').isVisible(), 'warns when storage is blocked');
  await openOne(p2); check(e2.length === 0, 'full pack works with storage blocked ' + JSON.stringify(e2));
  await c2.close();

  // 8. layouts: overflow check, several viewports
  for (const vp of [{ w: 320, h: 568 }, { w: 390, h: 844 }, { w: 768, h: 1024 }, { w: 1440, h: 900 }]) {
    const c = await b.newContext({ viewport: { width: vp.w, height: vp.h } }); const q = await c.newPage();
    await q.goto(url); await q.waitForTimeout(300);
    const ov1 = await q.evaluate(() => document.documentElement.scrollWidth - innerWidth);
    await q.screenshot({ path: out + `/07-open-${vp.w}.png` });
    await q.evaluate(() => { const s = window.__frc.S(); s.packs = 5; });
    await q.screenshot({ path: out + `/07b-shelf-${vp.w}.png` });
    await q.locator('.pick').nth(5).click({ force: true }); await waitState(q, 'inspect'); await q.waitForTimeout(600);
    const ovI = await q.evaluate(() => document.documentElement.scrollWidth - innerWidth);
    await q.screenshot({ path: out + `/07c-inspect-${vp.w}.png` }); await q.click('#backBtn'); await waitState(q, 'select');
    await openOne(q); await q.waitForTimeout(1200);
    const ov2 = await q.evaluate(() => document.documentElement.scrollWidth - innerWidth);
    await q.screenshot({ path: out + `/08-done-${vp.w}.png` });
    await q.click('#tabBinder'); await q.waitForTimeout(300);
    const ov3 = await q.evaluate(() => document.documentElement.scrollWidth - innerWidth);
    await q.screenshot({ path: out + `/09-binder-${vp.w}.png` });
    check(ov1 <= 0 && ovI <= 0 && ov2 <= 0 && ov3 <= 0, `no horizontal scroll at ${vp.w}px (${ov1},${ovI},${ov2},${ov3})`);
    await c.close();
  }
  await b.close();
  console.log(fails.length ? '\n' + fails.length + ' FAILED' : '\nALL PASSED');
  process.exit(fails.length ? 1 : 0);
})().catch(e => { console.error(e); process.exit(2); });
