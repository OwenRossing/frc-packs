// Shared helpers for the play-test bots.
const path = require('path');
exports.url = 'file://' + path.resolve(__dirname, '..', 'index.html');
exports.out = path.resolve(__dirname, 'out');
const st = p => p.evaluate(() => window.__frc.state());
exports.st = st;
exports.waitState = (p, s, timeout = 5000) => p.waitForFunction(s => window.__frc.state() === s, s, { timeout });

// Pick a pack from the shelf (unless one is already in hand), open it (swipe the top or press the button), then deal through the stack to the summary.
exports.openOne = async (p, { swipe = false, shelfIndex = 5, swipeCards = false } = {}) => {
  // "Open another pack" hands you a pack straight away, so only pick from the shelf when it is showing.
  if ((await st(p)) === 'select') await p.locator('.pick').nth(shelfIndex).click({ force: true });
  await exports.waitState(p, 'inspect');
  if (swipe) {
    await p.waitForTimeout(550);
    const b = await p.locator('#inspect').boundingBox();
    await p.mouse.move(b.x + b.width * .06, b.y + b.height * .12); await p.mouse.down();
    await p.mouse.move(b.x + b.width * .5, b.y + b.height * .12, { steps: 6 });
    await p.mouse.move(b.x + b.width * .96, b.y + b.height * .12, { steps: 6 });
    await p.mouse.up();
  } else await p.click('#openBtn');
  await exports.waitState(p, 'stack');
  for (let n = 0; n < 40 && (await st(p)) !== 'summary'; n++) {
    const s = await st(p);
    if (s === 'stack') {
      const hidden = await p.evaluate(() => document.querySelector('#stack').lastElementChild._hidden);
      if (swipeCards && !hidden) {
        const b = await p.locator('#stack').boundingBox();
        await p.mouse.move(b.x + b.width / 2, b.y + b.height / 2); await p.mouse.down();
        await p.mouse.move(b.x + b.width / 2 + 160, b.y + b.height / 2, { steps: 8 }); await p.mouse.up();
      } else await p.click('#stack', { force: true });
    }
    await p.waitForTimeout(200);
  }
  await exports.waitState(p, 'summary');
};
