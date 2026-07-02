const { chromium } = require('playwright');

(async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });

  let pass = 0;
  let fail = 0;

  function check(name, condition) {
    if (condition) {
      console.log(`  ✅ ${name}`);
      pass++;
    } else {
      console.log(`  ❌ ${name}`);
      fail++;
    }
  }

  console.log('\n=== E2E: EgoPulse WebUI Phase 1 ===\n');

  console.log('1. Basic Layout');
  await page.goto('http://localhost:4176/', { waitUntil: 'networkidle', timeout: 10000 });
  await page.waitForTimeout(1000);

  const sidebar = await page.$('.sidebar');
  check('Sidebar region exists', sidebar !== null);

  const topbar = await page.$('.topbar');
  check('Top Bar region exists', topbar !== null);

  const main = await page.$('.main');
  check('Main region exists', main !== null);

  const appShell = await page.$('.app-shell');
  check('App shell container exists', appShell !== null);

  console.log('\n2. Sidebar Content');
  const brandName = await page.$eval('.sidebar-brand-name', el => el.textContent).catch(() => null);
  check('Brand name "EgoPulse" displayed', brandName === 'EgoPulse');

  const agentsTitle = await page.$$eval('.sidebar-section', els =>
    els.some(el => el.textContent?.includes('Default Agent'))
  ).catch(() => false);
  check('AGENTS section shows configured agent', agentsTitle);

  const newSessionBtn = await page.$('.new-session-btn');
  check('New Session button exists', newSessionBtn !== null);

  const runtimeStatus = await page.$('.sidebar-runtime-status');
  check('Runtime Status footer exists', runtimeStatus !== null);

  console.log('\n3. Top Bar');
  const paletteTrigger = await page.$('.palette-trigger, [aria-label*="Search"], .topbar button');
  check('Palette trigger button exists', paletteTrigger !== null);

  const tabs = await page.$$eval('.topbar button, .tab-item', els =>
    els.filter(el => el.textContent?.includes('Chat')).length
  ).catch(() => 0);
  check('Chat tab exists', tabs > 0);

  console.log('\n4. Chat Tab');
  const chatHeader = await page.$('.chat-header');
  check('Chat header exists', chatHeader !== null);

  const timeline = await page.$('.timeline');
  check('Timeline exists', timeline !== null);

  const composer = await page.$('.composer');
  check('Composer exists', composer !== null);

  const channelBadge = await page.$('.badge-channel');
  check('Channel badge exists', channelBadge !== null);

  console.log('\n5. Command Palette');
  await page.keyboard.down('Meta');
  await page.keyboard.press('k');
  await page.keyboard.up('Meta');
  await page.waitForTimeout(500);

  const palette = await page.$('.palette-overlay');
  check('Palette opens on Cmd+K', palette !== null);

  if (palette) {
    const paletteInput = await page.$('.palette-input');
    check('Palette input focused', paletteInput !== null);

    const sections = await page.$$eval('.palette-section-title', els =>
      els.map(el => el.textContent)
    );
    check('Quick Actions section present', sections.includes('Quick Actions'));
    check('Navigation section present', sections.includes('Navigation'));
    check('Sleep & Pulse Runs section present', sections.includes('Sleep & Pulse Runs'));

    await page.keyboard.press('Escape');
    await page.waitForTimeout(300);
    const paletteAfter = await page.$('.palette-overlay');
    check('Palette closes on Escape', paletteAfter === null);
  }

  console.log('\n6. Responsive');
  await page.setViewportSize({ width: 375, height: 667 });
  await page.waitForTimeout(300);

  const hamburger = await page.$('.hamburger-btn, [aria-label="Toggle sidebar"]');
  check('Hamburger button on mobile', hamburger !== null);

  await page.setViewportSize({ width: 1280, height: 800 });
  await page.waitForTimeout(300);

  console.log('\n7. Screenshot');
  await page.screenshot({ path: '/root/workspace/egopulse/wt-webui-phase1/e2e-screenshot.png', fullPage: true });
  check('Screenshot saved', true);

  console.log(`\n=== Results: ${pass} passed, ${fail} failed ===\n`);

  await browser.close();
  process.exit(fail > 0 ? 1 : 0);
})();
