import { useEffect, useState } from 'react';

// Minimal hash router: "#/deals/D-abc" -> ["deals", "D-abc"].
const parse = () => location.hash.replace(/^#\/?/, '').split('/').filter(Boolean).map(decodeURIComponent);

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

export const navigate = (path: string) => {
  location.hash = path;
};

export const href = (path: string) => `#${path}`;
