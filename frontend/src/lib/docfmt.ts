// Formatting conventions for contract documents: amounts in words and dates,
// following Russian (ГОСТ Р 7.0.97-2016 practice) and US drafting conventions.

// ---- Russian -------------------------------------------------------------------

const RU_ONES_M = ['', 'один', 'два', 'три', 'четыре', 'пять', 'шесть', 'семь', 'восемь', 'девять'];
const RU_ONES_F = ['', 'одна', 'две', 'три', 'четыре', 'пять', 'шесть', 'семь', 'восемь', 'девять'];
const RU_TEENS = ['десять', 'одиннадцать', 'двенадцать', 'тринадцать', 'четырнадцать', 'пятнадцать', 'шестнадцать', 'семнадцать', 'восемнадцать', 'девятнадцать'];
const RU_TENS = ['', '', 'двадцать', 'тридцать', 'сорок', 'пятьдесят', 'шестьдесят', 'семьдесят', 'восемьдесят', 'девяносто'];
const RU_HUNDREDS = ['', 'сто', 'двести', 'триста', 'четыреста', 'пятьсот', 'шестьсот', 'семьсот', 'восемьсот', 'девятьсот'];

/** Russian plural form: 1 рубль, 2 рубля, 5 рублей. */
export function ruPlural(n: number, one: string, few: string, many: string) {
  const m10 = n % 10;
  const m100 = n % 100;
  if (m10 === 1 && m100 !== 11) return one;
  if (m10 >= 2 && m10 <= 4 && (m100 < 12 || m100 > 14)) return few;
  return many;
}

function ruTriad(n: number, feminine: boolean): string[] {
  const out: string[] = [];
  out.push(RU_HUNDREDS[Math.floor(n / 100)]);
  const t = n % 100;
  if (t >= 10 && t < 20) out.push(RU_TEENS[t - 10]);
  else {
    out.push(RU_TENS[Math.floor(t / 10)]);
    out.push((feminine ? RU_ONES_F : RU_ONES_M)[t % 10]);
  }
  return out.filter(Boolean);
}

export function ruNumberWords(n: number): string {
  if (n === 0) return 'ноль';
  const scales: [number, [string, string, string], boolean][] = [
    [1e9, ['миллиард', 'миллиарда', 'миллиардов'], false],
    [1e6, ['миллион', 'миллиона', 'миллионов'], false],
    [1e3, ['тысяча', 'тысячи', 'тысяч'], true],
  ];
  const words: string[] = [];
  let rest = n;
  for (const [size, forms, fem] of scales) {
    const k = Math.floor(rest / size);
    if (k) {
      words.push(...ruTriad(k, fem), ruPlural(k, ...forms));
      rest %= size;
    }
  }
  words.push(...ruTriad(rest, false));
  return words.join(' ');
}

/** 34899 → «348,99 DEAL (триста сорок восемь DEAL 99 центов)» */
export function ruAmount(minor: number): string {
  const whole = Math.floor(minor / 100);
  const cents = minor % 100;
  const digits = `${whole.toLocaleString('ru-RU')},${String(cents).padStart(2, '0')} DEAL`;
  const words = ruNumberWords(whole);
  return `${digits} (${words[0].toUpperCase() + words.slice(1)} DEAL ${String(cents).padStart(2, '0')} ${ruPlural(cents, 'цент', 'цента', 'центов')})`;
}

export const ruMoney = (minor: number) =>
  `${Math.floor(minor / 100).toLocaleString('ru-RU')},${String(minor % 100).padStart(2, '0')}`;

const RU_MONTHS = ['января', 'февраля', 'марта', 'апреля', 'мая', 'июня', 'июля', 'августа', 'сентября', 'октября', 'ноября', 'декабря'];
/** «28» сентября 2026 г. */
export const ruDateLong = (secs: number) => {
  const d = new Date(secs * 1000);
  return `«${String(d.getDate()).padStart(2, '0')}» ${RU_MONTHS[d.getMonth()]} ${d.getFullYear()} г.`;
};
/** 28.09.2026 (ГОСТ Р 7.0.97: ДД.ММ.ГГГГ) */
export const ruDate = (secs: number) => new Date(secs * 1000).toLocaleDateString('ru-RU');
export const ruDateTime = (secs: number) => new Date(secs * 1000).toLocaleString('ru-RU');

// ---- English (US) ------------------------------------------------------------------

const EN_ONES = ['', 'One', 'Two', 'Three', 'Four', 'Five', 'Six', 'Seven', 'Eight', 'Nine', 'Ten', 'Eleven', 'Twelve', 'Thirteen', 'Fourteen', 'Fifteen', 'Sixteen', 'Seventeen', 'Eighteen', 'Nineteen'];
const EN_TENS = ['', '', 'Twenty', 'Thirty', 'Forty', 'Fifty', 'Sixty', 'Seventy', 'Eighty', 'Ninety'];

function enTriad(n: number): string {
  const h = Math.floor(n / 100);
  const t = n % 100;
  const parts: string[] = [];
  if (h) parts.push(`${EN_ONES[h]} Hundred`);
  if (t >= 20) parts.push(EN_TENS[Math.floor(t / 10)] + (t % 10 ? '-' + EN_ONES[t % 10] : ''));
  else if (t) parts.push(EN_ONES[t]);
  return parts.join(' ');
}

export function enNumberWords(n: number): string {
  if (n === 0) return 'Zero';
  const scales: [number, string][] = [[1e9, 'Billion'], [1e6, 'Million'], [1e3, 'Thousand'], [1, '']];
  const out: string[] = [];
  let rest = n;
  for (const [size, name] of scales) {
    const k = Math.floor(rest / size);
    if (k) {
      out.push(enTriad(k) + (name ? ' ' + name : ''));
      rest %= size;
    }
  }
  return out.join(' ');
}

/** 34899 → "Three Hundred Forty-Eight and 99/100 DEAL (348.99 DEAL)" — check-writing style. */
export function enAmount(minor: number): string {
  const whole = Math.floor(minor / 100);
  const cents = String(minor % 100).padStart(2, '0');
  return `${enNumberWords(whole)} and ${cents}/100 DEAL (${enMoney(minor)} DEAL)`;
}

export const enMoney = (minor: number) =>
  (minor / 100).toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
/** September 28, 2026 */
export const enDate = (secs: number) =>
  new Date(secs * 1000).toLocaleDateString('en-US', { month: 'long', day: 'numeric', year: 'numeric' });
export const enDateTime = (secs: number) =>
  new Date(secs * 1000).toLocaleString('en-US', { month: 'short', day: 'numeric', year: 'numeric', hour: 'numeric', minute: '2-digit' });
