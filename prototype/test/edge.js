// Edge-case checks from the FMEA. Run: NODE_PATH=$(npm root -g) node test/edge.js
const { chromium } = require('playwright');
const path = require('path');
const url = 'file://' + path.resolve(__dirname, '..', 'index.html');
const out = path.resolve(__dirname, 'out');
const fails = [];
const check = (ok, msg) => { if (!ok) { fails.push(msg); console.log('FAIL', msg); } else console.log('ok  ', msg); };
const seed = (p, o) => p.evaluate(o => localStorage.setItem('frcpacks.v1', JSON.stringify(o)), o);
const base = (x = {}) => Object.assign({ v: 1, packs: 3, nextClaimAt: Date.now() + 3e6, inv: {}, pending: null, opened: 0, muted: true, demo: true }, x);

(async () => {
  const b = await chromium.launch({ executablePath: '/opt/pw-browsers/chromium' });

  // double click on the pack must spend exactly one pack
  { const c = await b.newContext({ viewport: { width: 400, height: 800 } }); const p = await c.newPage(); await p.goto(url); await seed(p, base()); await p.reload();
    await p.click('#pack', { force: true, clickCount: 2, delay: 5 }); await p.waitForTimeout(900);
    check((await p.evaluate(() => window.__frc.S().packs)) === 2, 'double-click spends one pack, not two'); await c.close(); }

  // two tabs: the stale tab must not erase the other tab's cards
  { const c = await b.newContext({ viewport: { width: 400, height: 800 } }); const a = await c.newPage(); await a.goto(url); await seed(a, base({ packs: 3 })); await a.reload();
    const t2 = await c.newPage(); await t2.goto(url);
    await a.bringToFront(); await a.click('#pack', { force: true }); await a.waitForFunction(() => window.__frc.state() === 'await-flip');
    for (let i = 0; i < 5; i++) { await a.click('#active', { force: true }); await a.waitForTimeout(900); if (i < 4) await a.click('#active', { force: true }); await a.waitForTimeout(200); }
    const cardsA = await a.evaluate(() => Object.values(window.__frc.S().inv).reduce((x, y) => x + y.length, 0));
    await t2.bringToFront(); await t2.waitForTimeout(200);
    await t2.click('#pack', { force: true }); await t2.waitForFunction(() => window.__frc.state() === 'await-flip');
    const st = await t2.evaluate(() => { const s = JSON.parse(localStorage.getItem('frcpacks.v1')); return { packs: s.packs, cards: Object.values(s.inv).reduce((x, y) => x + y.length, 0) }; });
    check(cardsA === 5 && st.cards === 10 && st.packs === 1, `second tab adds to the first tab's cards (5 then ${st.cards}, packs left ${st.packs})`); await c.close(); }

  // reduced motion
  { const c = await b.newContext({ viewport: { width: 400, height: 800 }, reducedMotion: 'reduce' }); const p = await c.newPage(); const e = []; p.on('pageerror', x => e.push(String(x)));
    await p.goto(url); await seed(p, base({ packs: 1 })); await p.reload();
    await p.click('#pack', { force: true }); await p.waitForFunction(() => window.__frc.state() === 'await-flip');
    for (let i = 0; i < 5; i++) { await p.click('#active', { force: true }); await p.waitForTimeout(900); if (i < 4) await p.click('#active', { force: true }); await p.waitForTimeout(150); }
    const anim = await p.evaluate(() => getComputedStyle(document.querySelector('.bg i')).animationName);
    check(e.length === 0 && anim === 'none', 'reduced motion: pack plays through and ambient animation is off'); await c.close(); }

  // keyboard only
  { const c = await b.newContext({ viewport: { width: 400, height: 800 } }); const p = await c.newPage(); await p.goto(url); await seed(p, base({ packs: 1 })); await p.reload();
    await p.focus('#pack'); await p.keyboard.press('Enter'); await p.waitForFunction(() => window.__frc.state() === 'await-flip', null, { timeout: 4000 });
    for (let i = 0; i < 5; i++) { await p.keyboard.press('Enter'); await p.waitForTimeout(900); if (i < 4) { await p.keyboard.press('Enter'); await p.waitForTimeout(200); } }
    check((await p.evaluate(() => window.__frc.state())) === 'done', 'a whole pack can be opened with the keyboard alone');
    check((await p.evaluate(() => document.activeElement && document.activeElement.id)) === 'again', 'focus lands on the next action when the pack ends'); await c.close(); }

  // touch
  { const c = await b.newContext({ viewport: { width: 390, height: 800 }, hasTouch: true, isMobile: true }); const p = await c.newPage(); await p.goto(url); await seed(p, base({ packs: 1 })); await p.reload();
    const bb = await p.locator('#pack').boundingBox(); await p.touchscreen.tap(bb.x + bb.width / 2, bb.y + bb.height / 2);
    await p.waitForFunction(() => window.__frc.state() === 'await-flip', null, { timeout: 4000 }).then(() => check(true, 'tapping the pack opens it on touch'), () => check(false, 'tapping the pack opens it on touch')); await c.close(); }

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
