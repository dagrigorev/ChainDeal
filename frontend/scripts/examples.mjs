// Generates example contract documents (PDF + PNG preview) and verification
// screenshots into docs/examples/, using Chrome's print engine — the same
// output as the document page's "Print / Save PDF" button.
//
//   make examples      (needs `make web` running and `make seed` loaded)
import { chromium } from 'playwright-core';
import { mkdirSync, writeFileSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const OUT = ROOT + '/docs/examples';
const UI = 'http://localhost:5173';
const API = 'https://chaindeal.localhost/api';
mkdirSync(OUT, { recursive: true });

const get = async (p) => {
  const r = await fetch(API + p);
  if (!r.ok) throw new Error(`${p}: ${r.status}`);
  return r.json();
};
const demo = JSON.parse(readFileSync(ROOT + '/scripts/demo-wallets.json', 'utf8'));
const byName = (n) => demo.find((w) => w.name.startsWith(n)).address;

// Pick representative deals: seeded ones plus a failed deal from the history.
const alice = (await get(`/accounts/${byName('Alice')}`)).deals;
const acme = (await get(`/accounts/${byName('Acme')}`)).deals;
const pick = (list, title, status) => list.find((d) => d.title.startsWith(title) && (!status || d.status === status));
const examples = [
  { slug: 'c2c-completed', deal: pick(alice, 'Vintage road bike', 'completed'), stds: ['ru', 'us'] },
  { slug: 'b2b-dispute-resolved', deal: pick(acme, 'Q4 server rack', 'resolved'), stds: ['ru', 'us'] },
  { slug: 'b2b-seller-default', deal: (await get('/deals?limit=1&type=B2B&status=failed'))[0], stds: ['ru', 'us'] },
  { slug: 'b2c-open-draft', deal: pick(alice, 'Noise-cancelling'), stds: ['ru'] },
];

const browser = await chromium.launch({ executablePath: process.env.CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
// Reduced motion disables the page entrance animation, so captures are never mid-fade.
const ctx = await browser.newContext({ viewport: { width: 1280, height: 1000 }, colorScheme: 'light', reducedMotion: 'reduce' });
const page = await ctx.newPage();

for (const ex of examples) {
  if (!ex.deal) {
    console.warn(`  skipped ${ex.slug}: deal not found (run make seed)`);
    continue;
  }
  for (const std of ex.stds) {
    const base = `${OUT}/${ex.slug}-${std}`;
    await page.goto(`${UI}/#/deals/${ex.deal.id}/document?std=${std}`);
    await page.waitForSelector('.doc-sheet .doc-cert img');
    await page.waitForTimeout(800);
    await page.emulateMedia({ media: 'print' });
    await page.pdf({ path: `${base}.pdf`, preferCSSPageSize: true, printBackground: true });
    await page.emulateMedia({ media: 'screen' });
    // PNG preview of the first sheet (the paper as shown on screen).
    const box = await page.locator('.doc-sheet').boundingBox();
    const pageHeight = std === 'ru' ? 1123 : 1056; // A4 / Letter at 96 dpi
    await page.screenshot({ path: `${base}.png`, clip: { x: box.x, y: box.y, width: box.width, height: Math.min(box.height, pageHeight) } });
    console.log(`  ${ex.slug}-${std}.pdf/.png  (${ex.deal.id}, ${ex.deal.status})`);
  }
}

// Verification: a genuine document, and the same document with one hex digit changed.
const doc = await get(`/deals/${examples[1].deal.id}/document`);
writeFileSync(`${OUT}/b2b-dispute-resolved.document.json`, JSON.stringify(doc, null, 2));
writeFileSync(`${OUT}/b2b-dispute-resolved.verification.json`,
  JSON.stringify(await get(`/documents/verify?deal=${doc.record.deal_id}&hash=${doc.hash}&sig=${doc.attestation.signature}`), null, 2));
await page.goto(UI + doc.verify_path.replace('/#', '/#'));
await page.waitForSelector('.verdict');
await page.waitForTimeout(1500);
await page.screenshot({ path: `${OUT}/verify-authentic.png`, fullPage: true });
const tampered = doc.verify_path.replace(`hash=${doc.hash}`, `hash=${(doc.hash[0] === '0' ? '1' : '0') + doc.hash.slice(1)}`);
await page.goto(UI + tampered);
await page.waitForSelector('.verdict');
await page.waitForTimeout(1500);
await page.screenshot({ path: `${OUT}/verify-tampered.png`, fullPage: true });
console.log('  verify-authentic.png, verify-tampered.png, *.document.json, *.verification.json');

await browser.close();
