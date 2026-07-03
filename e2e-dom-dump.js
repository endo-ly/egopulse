const { chromium } = require('playwright');

(async () => {
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  await page.goto('http://localhost:4176/', { waitUntil: 'networkidle' });
  await page.waitForTimeout(1500);

  const dump = await page.evaluate(() => {
    function describe(sel) {
      const el = document.querySelector(sel);
      if (!el) return { sel, found: false };
      const s = getComputedStyle(el);
      const r = el.getBoundingClientRect();
      return {
        sel,
        found: true,
        tag: el.tagName,
        text: el.textContent?.substring(0, 80),
        box: { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) },
        styles: {
          display: s.display,
          position: s.position,
          gridTemplateRows: s.gridTemplateRows,
          gridTemplateColumns: s.gridTemplateColumns,
          flexDirection: s.flexDirection,
          background: s.backgroundColor,
          color: s.color,
          fontSize: s.fontSize,
          padding: s.padding,
          gap: s.gap,
          border: s.border,
          overflow: s.overflow,
          height: s.height,
          maxHeight: s.maxHeight,
        },
      };
    }

    const result = {};
    const selectors = [
      '.app-shell',
      '.sidebar', '.sidebar-nav',
      '.topbar',
      '.main',
      '.chat-tab',
      '.chat-header', '.chat-header-label', '.chat-header-meta',
      '.timeline',
      '.composer', '.composer-input-wrapper', '.composer-textarea', '.composer-send',
      '.message-row', '.message-body',
      '.sidebar-brand', '.sidebar-brand-name',
      '.sidebar-section',
      '.agent-row',
      '.session-item',
      '.palette-trigger',
      '.tab',
      '.sidebar-runtime-status',
      '.new-session-btn',
    ];
    for (const sel of selectors) {
      result[sel] = describe(sel);
    }

    const allMsgs = document.querySelectorAll('.message-row');
    result._messages = [];
    for (const m of allMsgs) {
      const s = getComputedStyle(m);
      const r = m.getBoundingClientRect();
      result._messages.push({
        class: m.className,
        text: m.textContent?.substring(0, 60),
        align: s.alignSelf,
        box: { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) },
      });
    }

    result._bodyBg = getComputedStyle(document.body).backgroundColor;
    result._bodyFont = getComputedStyle(document.body).fontFamily;

    return result;
  });

  console.log(JSON.stringify(dump, null, 2));

  await page.screenshot({ path: 'e2e-screenshot.png', fullPage: false });
  await browser.close();
})();
