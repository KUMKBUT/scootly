const { chromium } = require('playwright');
const path = require('path');

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage({
    viewport: { width: 1920, height: 1080 },
    deviceScaleFactor: 4,
  });
  await page.goto('file://' + path.join(__dirname, 'index.html'), {
    waitUntil: 'networkidle',
    timeout: 60000,
  });
  await page.evaluate(() => document.fonts.ready);
  await page.evaluate(async () => {
    const broken = await Promise.all(
      Array.from(document.images).map(async (img) => {
        try {
          await img.decode();
          return null;
        } catch {
          return img.src;
        }
      })
    );
    const failed = broken.filter(Boolean);
    if (failed.length > 0) {
      throw new Error('Failed to load images: ' + failed.join(', '));
    }
  });
  await page.waitForTimeout(500);
  const out = path.join(
    __dirname,
    '..',
    '..',
    'docs',
    'architecture',
    'exports',
    'poster-telegram.png'
  );
  await page.screenshot({ path: out });
  await browser.close();
  console.log('OK: ' + out);
})().catch((err) => {
  console.error(err);
  process.exit(1);
});
