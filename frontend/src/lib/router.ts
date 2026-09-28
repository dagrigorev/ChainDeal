import { useEffect, useState } from 'react';

// Minimal hash router: "#/deals/D-abc?std=ru" -> ["deals", "D-abc"] + query {std: "ru"}.
const split = () => {
  const raw = location.hash.replace(/^#\/?/, '');
  const q = raw.indexOf('?');
  return { path: q < 0 ? raw : raw.slice(0, q), query: q < 0 ? '' : raw.slice(q + 1) };
};
const parse = () => split().path.split('/').filter(Boolean).map(decodeURIComponent);

export function useRoute(): string[] {
  const [route, setRoute] = useState(parse);
  useEffect(() => {
    const on = () => {
      setRoute(parse());
      window.scrollTo(0, 0);
    };
    window.addEventListener('hashchange', on);
    return () => window.removeEventListener('hashchange', on);
  }, []);
  return route;
}

/** Query parameters of the current hash route. */
export function useQuery(): URLSearchParams {
  const [q, setQ] = useState(() => new URLSearchParams(split().query));
  useEffect(() => {
    const on = () => setQ(new URLSearchParams(split().query));
    window.addEventListener('hashchange', on);
    return () => window.removeEventListener('hashchange', on);
  }, []);
  return q;
}

export const navigate = (path: string) => {
  location.hash = path;
};

export const href = (path: string) => `#${path}`;
