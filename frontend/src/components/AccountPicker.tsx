import { useEffect, useId, useRef, useState } from 'react';
import { api } from '../lib/api';
import { useStore } from '../lib/store';
import type { Account } from '../lib/types';
import { Avatar, KindTag } from './ui';

interface Props {
  label: string;
  value: string;
  onChange(address: string): void;
  exclude?: string[];
  placeholder?: string;
  /** Text for the "none" option; omit to require a choice. */
  noneLabel?: string;
}

/**
 * Searchable account combobox (WAI-ARIA combobox pattern). Replaces <select>
 * lists, which don't scale to a network with thousands of participants.
 */
export default function AccountPicker({ label, value, onChange, exclude = [], placeholder = 'Search by name or address…', noneLabel }: Props) {
  const { directory, resolve } = useStore();
  const id = useId();
  const [q, setQ] = useState('');
  const [open, setOpen] = useState(false);
  const [items, setItems] = useState<Account[]>([]);
  const [total, setTotal] = useState(0);
  const [cursor, setCursor] = useState(0);
  const box = useRef<HTMLDivElement>(null);
  const selected = value ? directory.get(value) : undefined;

  useEffect(() => {
    if (value) resolve([value]);
  }, [value, resolve]);

  // Debounced server-side search.
  useEffect(() => {
    if (!open) return;
    const t = setTimeout(() => {
      api.searchAccounts(q.trim(), '', 0, 12)
        .then((r) => {
          setItems(r.items.filter((a) => !exclude.includes(a.address)));
          setTotal(r.total);
          setCursor(0);
        })
        .catch(() => {});
    }, 150);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [q, open, exclude.join(',')]);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => box.current && !box.current.contains(e.target as Node) && setOpen(false);
    document.addEventListener('mousedown', close);
    return () => document.removeEventListener('mousedown', close);
  }, [open]);

  const options: (Account | null)[] = noneLabel ? [null, ...items] : items;
  const choose = (a: Account | null) => {
    onChange(a?.address ?? '');
    setQ('');
    setOpen(false);
  };

  return (
    <div className="picker" ref={box}>
      <label htmlFor={id}>{label}</label>
      <div className="picker-field">
        {selected && !open && (
          <button type="button" className="picker-chosen" onClick={() => setOpen(true)} aria-label={`${label}: ${selected.name}. Change`}>
            <Avatar a={selected.address} size={18} /> <span>{selected.name}</span> <KindTag kind={selected.kind} />
          </button>
        )}
        <input
          id={id}
          role="combobox"
          aria-expanded={open}
          aria-controls={`${id}-list`}
          aria-autocomplete="list"
          aria-activedescendant={open && options[cursor] !== undefined ? `${id}-opt-${cursor}` : undefined}
          className={selected && !open ? 'picker-hidden' : ''}
          value={q}
          placeholder={selected ? selected.name : noneLabel && !value ? noneLabel : placeholder}
          onFocus={() => setOpen(true)}
          onChange={(e) => {
            setQ(e.target.value);
            setOpen(true);
          }}
          onKeyDown={(e) => {
            if (e.key === 'ArrowDown') { e.preventDefault(); setOpen(true); setCursor((c) => Math.min(c + 1, options.length - 1)); }
            else if (e.key === 'ArrowUp') { e.preventDefault(); setCursor((c) => Math.max(c - 1, 0)); }
            else if (e.key === 'Enter' && open && options.length) { e.preventDefault(); choose(options[cursor]); }
            else if (e.key === 'Escape') setOpen(false);
          }}
        />
      </div>
      {open && (
        <ul className="picker-list" id={`${id}-list`} role="listbox" aria-label={label}>
          {options.map((a, i) => (
            <li
              key={a?.address ?? 'none'}
              id={`${id}-opt-${i}`}
              role="option"
              aria-selected={i === cursor}
              className={i === cursor ? 'on' : ''}
              onMouseDown={(e) => { e.preventDefault(); choose(a); }}
              onMouseEnter={() => setCursor(i)}
            >
              {a ? (
                <>
                  <Avatar a={a.address} size={18} />
                  <span className="grow ellipsis">{a.name}</span>
                  <KindTag kind={a.kind} />
                  <span className="muted tiny">{a.deals_completed} deals</span>
                </>
              ) : <span className="muted">{noneLabel}</span>}
            </li>
          ))}
          {items.length === 0 && <li className="muted picker-empty" role="presentation">No matches</li>}
          {total > items.length && <li className="muted tiny picker-empty" role="presentation">{total.toLocaleString()} matches — keep typing to narrow</li>}
        </ul>
      )}
    </div>
  );
}
