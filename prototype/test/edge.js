// Edge-case checks from the FMEA. Run: NODE_PATH=$(npm root -g) node test/edge.js
const { chromium } = require('playwright');
const path = require('path');
const { url, out, openOne, waitState } = require('./lib');
const fails = [];
const check = (ok, msg) => { if (!ok) { fails.push(msg); console.log('FAIL', msg); } else console.log('ok  ', msg); };
const seed = (p, o) => p.evaluate(o => localStorage.setItem('frcpacks.cmp26', JSON.stringify(o)), o);
const base = (x = {}) => Object.assign({ v: 1, packs: 3, nextClaimAt: Date.now() + 3e6, inv: {}, pending: null, opened: 0, muted: true, demo: true }, x);

(async () => {
  const b = await chromium.launch({ executablePath: '/opt/pw-browsers/chromium' });

  // double click on the open button must spend exactly one pack
  { const c = await b.newContext({ viewport: { width: 400, height: 800 } }); const p = await c.newPage(); await p.goto(url); await seed(p, base()); await p.reload();
    await p.locator('.pick').nth(5).dblclick({ force: true }); await waitState(p, 'inspect');
    await p.click('#openBtn', { clickCount: 2, delay: 5 }); await p.waitForTimeout(900);
    check((await p.evaluate(() => window.__frc.S().packs)) === 2, 'double-click spends one pack, not two'); await c.close(); }

  // a short tap on the cut zone, or a swipe while the pack faces away, must not open it
  { const c = await b.newContext({ viewport: { width: 400, height: 800 } }); const p = await c.newPage(); await p.goto(url); await seed(p, base()); await p.reload();
    await p.locator('.pick').nth(5).click({ force: true }); await waitState(p, 'inspect'); await p.waitForTimeout(600);
    const bb = await p.locator('#inspect').boundingBox();
    await p.mouse.click(bb.x + bb.width / 2, bb.y + bb.height * .12); await p.waitForTimeout(200);
    await p.mouse.move(bb.x + bb.width * .1, bb.y + bb.height * .12); await p.mouse.down(); await p.mouse.move(bb.x + bb.width * .4, bb.y + bb.height * .12, { steps: 5 }); await p.mouse.up(); await p.waitForTimeout(200);
    await p.click('#turnBtn'); await p.waitForTimeout(600);
    await p.mouse.move(bb.x + bb.width * .06, bb.y + bb.height * .12); await p.mouse.down(); await p.mouse.move(bb.x + bb.width * .96, bb.y + bb.height * .12, { steps: 8 }); await p.mouse.up(); await p.waitForTimeout(600);
    check((await p.evaluate(() => ({ s: window.__frc.state(), n: window.__frc.S().packs }))).n === 3, 'taps, short swipes and swipes on the back do not open the pack');
    await p.keyboard.press('Escape'); await p.waitForTimeout(100);
    check((await p.evaluate(() => window.__frc.state())) === 'select', 'escape puts the pack back on the shelf'); await c.close(); }

  // face-down cards resist a swipe and need a tap
  { const c = await b.newContext({ viewport: { width: 400, height: 800 } }); const p = await c.newPage(); await p.goto(url); await seed(p, base()); await p.reload();
    await p.locator('.pick').nth(5).click({ force: true }); await waitState(p, 'inspect'); await p.click('#openBtn'); await waitState(p, 'stack');
    for (let i = 0; i < 4 && !(await p.evaluate(() => document.querySelector('#stack').lastElementChild._hidden)); i++) { await p.click('#stack', { force: true }); await p.waitForTimeout(600); }
    const before = await p.evaluate(() => window.__frc.S().pending.revealed);
    const bb = await p.locator('#stack').boundingBox();
    await p.mouse.move(bb.x + bb.width / 2, bb.y + bb.height / 2); await p.mouse.down(); await p.mouse.move(bb.x + bb.width / 2 + 200, bb.y + bb.height / 2, { steps: 8 }); await p.mouse.up(); await p.waitForTimeout(600);
    check((await p.evaluate(() => window.__frc.S().pending.revealed)) === before, 'swiping a face-down card does not skip it');
    await p.click('#stack', { force: true }); await p.waitForTimeout(1000);
    await p.screenshot({ path: out + '/12-stack-reveal.png' });
    check(!(await p.evaluate(() => document.querySelector('#stack').lastElementChild._hidden)), 'tapping flips it'); await c.close(); }

  // two tabs: the stale tab must not erase the other tab's cards
  { const c = await b.newContext({ viewport: { width: 400, height: 800 } }); const a = await c.newPage(); await a.goto(url); await seed(a, base({ packs: 3 })); await a.reload();
    const t2 = await c.newPage(); await t2.goto(url);
    await a.bringToFront(); await openOne(a);
    const cardsA = await a.evaluate(() => Object.values(window.__frc.S().inv).reduce((x, y) => x + y.length, 0));
    await t2.bringToFront(); await t2.waitForTimeout(200);
    await t2.locator('.pick').nth(5).click({ force: true }); await waitState(t2, 'inspect'); await t2.click('#openBtn'); await waitState(t2, 'stack');
    const st = await t2.evaluate(() => { const s = JSON.parse(localStorage.getItem('frcpacks.cmp26')); return { packs: s.packs, cards: Object.values(s.inv).reduce((x, y) => x + y.length, 0) }; });
    check(cardsA === 5 && st.cards === 10 && st.packs === 1, `second tab adds to the first tab's cards (5 then ${st.cards}, packs left ${st.packs})`); await c.close(); }

  // reduced motion
  { const c = await b.newContext({ viewport: { width: 400, height: 800 }, reducedMotion: 'reduce' }); const p = await c.newPage(); const e = []; p.on('pageerror', x => e.push(String(x)));
    await p.goto(url); await seed(p, base({ packs: 1 })); await p.reload();
    await openOne(p, { swipe: true });
    const anim = await p.evaluate(() => getComputedStyle(document.querySelector('.bg i')).animationName);
    check(e.length === 0 && anim === 'none', 'reduced motion: pack plays through and ambient animation is off'); await c.close(); }

  // keyboard only
  { const c = await b.newContext({ viewport: { width: 400, height: 800 } }); const p = await c.newPage(); await p.goto(url); await seed(p, base({ packs: 1 })); await p.reload();
    await p.locator('.pick').nth(5).focus(); await p.keyboard.press('Enter'); await waitState(p, 'inspect', 4000);
    await p.keyboard.press('Enter'); await waitState(p, 'stack', 4000);
    for (let i = 0; i < 15 && (await p.evaluate(() => window.__frc.state())) !== 'summary'; i++) { await p.keyboard.press('Enter'); await p.waitForTimeout(950); }
    check((await p.evaluate(() => window.__frc.state())) === 'summary', 'a whole pack can be opened with the keyboard alone');
    check((await p.evaluate(() => document.activeElement && document.activeElement.id)) === 'again', 'focus lands on the next action when the pack ends'); await c.close(); }

  // touch
  { const c = await b.newContext({ viewport: { width: 390, height: 800 }, hasTouch: true, isMobile: true }); const p = await c.newPage(); await p.goto(url); await seed(p, base({ packs: 1 })); await p.reload(); await p.waitForTimeout(300);
    const bb = await p.locator('.pick').nth(5).boundingBox(); await p.touchscreen.tap(bb.x + bb.width / 2, bb.y + bb.height / 2);
    let ok = await waitState(p, 'inspect', 4000).then(() => true, () => false);
    if (ok) { const ob = await p.locator('#openBtn').boundingBox(); await p.touchscreen.tap(ob.x + ob.width / 2, ob.y + ob.height / 2); ok = await waitState(p, 'stack', 4000).then(() => true, () => false); }
    if (ok) { const sb = await p.locator('#stack').boundingBox(); await p.touchscreen.tap(sb.x + sb.width / 2, sb.y + sb.height / 2); await p.waitForTimeout(700); ok = (await p.evaluate(() => window.__frc.S().pending.revealed)) === 1; }
    check(ok, 'tapping picks, opens and deals on touch'); await c.close(); }

  // garbage and hostile saves
  { const c = await b.newContext({ viewport: { width: 400, height: 800 } }); const p = await c.newPage(); const e = []; p.on('pageerror', x => e.push(String(x))); await p.goto(url);
    for (const bad of [{ packs: 'many' }, { packs: -4 }, { packs: 1e9 }, { nextClaimAt: 'soon' }, { inv: [] }, { inv: { '254': 'x' } }]) {
      await seed(p, base(bad)); await p.reload(); await p.waitForTimeout(150);
      const ok = await p.evaluate(() => { const s = window.__frc.S(); return Number.isFinite(s.packs) && s.packs >= 0 && s.packs < 10000; });
      check(ok && e.length === 0, 'bad save ignored: ' + JSON.stringify(bad));
    }
    await seed(p, base({ inv: { '4414': ['<img src=x onerror=window.__pwn=1>', '000417'] } })); await p.reload(); await p.click('#tabBinder'); await p.waitForTimeout(200);
    await p.locator('#grid .slot').first().click(); await p.waitForTimeout(200);
    check(!(await p.evaluate(() => window.__pwn)), 'hostile serial in storage is dropped or escaped');
    check((await p.evaluate(() => window.__frc.S().inv['4414'].length)) === 1, 'only valid serials are kept'); await c.close(); }

  // big binder + long names
  { const c = await b.newContext({ viewport: { width: 400, height: 900 } }); const p = await c.newPage(); await p.goto(url);
    const inv = await p.evaluate(() => { const o = {}; window.__frc.TEAMS.forEach((t, i) => { o[t.num] = ['%06d'.replace('%06d', String(100000 + i))]; }); return o; });
    await seed(p, base({ inv })); await p.reload(); const t0 = Date.now(); await p.click('#tabBinder'); await p.waitForSelector('#grid .slot'); const dt = Date.now() - t0;
    const n = await p.locator('#grid .slot').count(); check(n === 48 && dt < 1500, `binder with ${Object.keys(inv).length} teams renders ${n} cards in ${dt}ms`);
    await p.click('#more'); check((await p.locator('#grid .slot').count()) === 96, 'show more adds another page');
    // longest names
    const long = await p.evaluate(() => window.__frc.TEAMS.slice().sort((a, b) => b.name.length - a.name.length).slice(0, 3).map(t => t.num));
    await seed(p, base({ inv: Object.fromEntries(long.map(n => [n, ['000001']])) })); await p.reload(); await p.click('#tabBinder'); await p.waitForTimeout(300);
    await p.screenshot({ path: out + '/10-long-names.png' });
    await p.locator('#grid .slot').first().click(); await p.waitForTimeout(300); await p.screenshot({ path: out + '/11-long-inspect.png' }); await c.close(); }

  await b.close();
  console.log(fails.length ? '\n' + fails.length + ' FAILED' : '\nALL PASSED'); process.exit(fails.length ? 1 : 0);
})().catch(e => { console.error(e); process.exit(2); });
