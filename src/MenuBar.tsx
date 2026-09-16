import { useCallback, useEffect, useRef, useState } from "react";
import {
  buildMenuTree,
  menuSignature,
  type MenuColor,
  type MenuEntry,
  type MenuGroup,
  type MenuItem,
} from "./menuBar";

/**
 * The commands the toolbar carries, read from its buttons: every button
 * whose title names a Photoshop menu (`Edit > Transform > Rotate…`) and
 * that is not a tool. Choosing the menu item clicks the button, so the
 * button's own handler, disabled state, and toggle state are the
 * command's — the toolbar stays the one registry of what the app does.
 */
export function toolbarEntries(root: ParentNode): MenuEntry[] {
  return Array.from(root.querySelectorAll<HTMLButtonElement>("header.toolbar button"))
    .filter((button) => button.dataset.tool === undefined && button.title.includes(" > "))
    .map((button) => {
      const pressed = button.getAttribute("aria-pressed");
      return {
        label: (button.textContent ?? "").replace(/\s+/g, " ").trim(),
        title: button.title,
        disabled: button.disabled,
        checked: pressed === null ? null : pressed === "true",
        run: () => button.click(),
      };
    });
}

type Props = {
  /** Read whenever a menu is open, after every render, so states stay live. */
  entries: () => MenuEntry[];
  /** Edit > Menus: the `commandKey`s left out of the menus. */
  hidden: ReadonlySet<string>;
  /** Edit > Menus: each coloured command's own Menu Color, by `commandKey`. */
  colors: ReadonlyMap<string, MenuColor>;
};

const ITEM_SELECTOR = '[role="menuitem"]:not(:disabled), [role="menuitemcheckbox"]:not(:disabled)';

function siblingsOf(element: HTMLElement): HTMLElement[] {
  const list = element.closest("ul");
  if (!list) return [];
  return Array.from(
    list.querySelectorAll<HTMLElement>(
      ":scope > li > " + ITEM_SELECTOR.replace(/, /g, ", :scope > li > "),
    ),
  );
}

/**
 * A Photoshop-style menu bar: File … Help across the top, each opening a
 * dropdown of the commands the toolbar already offers, with submenus,
 * disabled states, toggle marks, and the `(Ctrl/Cmd+…)` hints their
 * titles carry. Mouse: click a menu to open it, slide across to switch,
 * click a command to run it. Keyboard: Left/Right switch menus, Up/Down
 * move, Right opens a submenu, Left closes it, Enter runs, Escape closes.
 */
export default function MenuBar({ entries, hidden, colors }: Props) {
  const [open, setOpen] = useState<number | null>(null);
  const [tree, setTree] = useState<MenuGroup[]>(() => buildMenuTree([]));
  // Where each open submenu sits, in viewport pixels: submenus are
  // positioned fixed so a scrolling dropdown (Filter's 161 rows) cannot
  // clip them -- found by driving the real app under Xvfb (README Phase
  // 362), where every submenu was invisible.
  const [submenuAt, setSubmenuAt] = useState<Record<string, { top: number; left: number }>>({});
  const placeSubmenu = useCallback((key: string, element: HTMLElement) => {
    const rect = element.getBoundingClientRect();
    setSubmenuAt((previous) => {
      const next = { top: rect.top - 5, left: rect.right };
      const current = previous[key];
      if (current && current.top === next.top && current.left === next.left) return previous;
      return { ...previous, [key]: next };
    });
  }, []);
  const signature = useRef("");
  const nav = useRef<HTMLElement>(null);

  // Rebuild from the toolbar after every commit while a menu is open, so a
  // command that just became available (or busy) reads that way at once.
  // The signature check keeps an unchanged toolbar from re-rendering.
  useEffect(() => {
    if (open === null) return;
    const current = entries();
    const colorSignature = [...colors]
      .map(([key, color]) => key + "\u0004" + color)
      .join("\u0001");
    const next =
      menuSignature(current) +
      "\u0002" +
      [...hidden].join("\u0001") +
      "\u0003" +
      colorSignature;
    if (next === signature.current) return;
    signature.current = next;
    setTree(buildMenuTree(current, hidden, colors));
  });

  const close = useCallback(() => {
    setOpen(null);
    signature.current = "";
  }, []);

  useEffect(() => {
    if (open === null) return;
    const onPointerDown = (event: PointerEvent) => {
      if (nav.current && event.target instanceof Node && nav.current.contains(event.target)) return;
      close();
    };
    const onBlur = () => close();
    document.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("blur", onBlur);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("blur", onBlur);
    };
  }, [open, close]);

  const focusTop = useCallback((index: number) => {
    const buttons = nav.current?.querySelectorAll<HTMLButtonElement>(".menubar__menu");
    buttons?.[index]?.focus();
  }, []);

  const switchTo = useCallback(
    (index: number) => {
      const wrapped = (index + tree.length) % tree.length;
      setOpen(wrapped);
      focusTop(wrapped);
    },
    [tree.length, focusTop],
  );

  const onKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLElement>) => {
      if (open === null) {
        if (event.key === "ArrowDown" || event.key === "Enter" || event.key === " ") {
          const target = event.target as HTMLElement;
          const index = Number(target.dataset.menuIndex);
          if (!Number.isNaN(index)) {
            event.preventDefault();
            setOpen(index);
          }
        }
        return;
      }
      const target = event.target as HTMLElement;
      const inItem = target.matches(ITEM_SELECTOR) || target.classList.contains("menubar__group");
      switch (event.key) {
        case "Escape": {
          event.preventDefault();
          focusTop(open);
          close();
          return;
        }
        case "ArrowLeft": {
          event.preventDefault();
          const submenu = target.closest("ul.menubar__submenu");
          if (inItem && submenu) {
            (
              submenu.parentElement?.querySelector(".menubar__group") as HTMLElement | null
            )?.focus();
          } else {
            switchTo(open - 1);
          }
          return;
        }
        case "ArrowRight": {
          event.preventDefault();
          if (target.classList.contains("menubar__group")) {
            const first = target.parentElement?.querySelector<HTMLElement>(
              "ul > li > " + ITEM_SELECTOR.split(", ")[0],
            );
            if (first) {
              first.focus();
              return;
            }
          }
          switchTo(open + 1);
          return;
        }
        case "ArrowDown":
        case "ArrowUp": {
          event.preventDefault();
          const step = event.key === "ArrowDown" ? 1 : -1;
          if (inItem) {
            const items = siblingsOf(target);
            const at = items.indexOf(target);
            const next = items[(at + step + items.length) % items.length];
            next?.focus();
          } else {
            const items = nav.current?.querySelectorAll<HTMLElement>(
              ".menubar__dropdown > li > " +
                ITEM_SELECTOR.replace(/, /g, ", .menubar__dropdown > li > "),
            );
            items?.[step === 1 ? 0 : items.length - 1]?.focus();
          }
          return;
        }
        default:
          return;
      }
    },
    [open, close, focusTop, switchTo],
  );

  const renderItems = (items: MenuItem[], depth: number) => {
    if (items.length === 0) {
      return (
        <li className="menubar__item">
          <button type="button" role="menuitem" className="menubar__command" disabled>
            <span className="menubar__label">No commands yet</span>
          </button>
        </li>
      );
    }
    return items.map((item, index) =>
      item.kind === "group" ? (
        <li
          className="menubar__item menubar__item--group"
          key={`${item.label}-${index}`}
          onPointerEnter={(event) => placeSubmenu(item.path.join(">"), event.currentTarget)}
          onFocus={(event) => placeSubmenu(item.path.join(">"), event.currentTarget)}
        >
          <button
            type="button"
            className="menubar__command menubar__group"
            aria-haspopup="menu"
            tabIndex={-1}
            onClick={(event) => event.stopPropagation()}
          >
            <span className="menubar__check" aria-hidden="true" />
            <span className="menubar__label">{item.label}</span>
            <span className="menubar__arrow" aria-hidden="true">
              ▸
            </span>
          </button>
          <ul
            role="menu"
            className="menubar__submenu"
            aria-label={item.path.join(" > ")}
            style={(() => {
              const at = submenuAt[item.path.join(">")];
              return at
                ? {
                    position: "fixed" as const,
                    top: at.top,
                    left: at.left,
                    maxHeight: Math.max(120, window.innerHeight - at.top - 8),
                  }
                : undefined;
            })()}
          >
            {renderItems(item.items, depth + 1)}
          </ul>
        </li>
      ) : (
        <li className="menubar__item" key={`${item.label}-${index}`}>
          <button
            type="button"
            role={item.checked === null ? "menuitem" : "menuitemcheckbox"}
            aria-checked={item.checked === null ? undefined : item.checked}
            className="menubar__command"
            disabled={item.disabled}
            title={item.hint || undefined}
            tabIndex={-1}
            onClick={() => {
              close();
              item.run();
            }}
          >
            <span className="menubar__check" aria-hidden="true">
              {item.checked ? "✓" : ""}
            </span>
            {item.color && (
              <span
                className={`menubar__color-swatch menubar__color-swatch--${item.color}`}
                aria-hidden="true"
              />
            )}
            <span className="menubar__label">{item.label}</span>
            {item.shortcut && <kbd className="menubar__shortcut">{item.shortcut}</kbd>}
          </button>
        </li>
      ),
    );
  };

  return (
    <nav className="menubar" role="menubar" aria-label="Menu bar" ref={nav} onKeyDown={onKeyDown}>
      {tree.map((menu, index) => (
        <div
          className={`menubar__entry${open === index ? " menubar__entry--open" : ""}`}
          key={menu.label}
        >
          <button
            type="button"
            className="menubar__menu"
            role="menuitem"
            aria-haspopup="menu"
            aria-expanded={open === index}
            data-menu-index={index}
            onClick={() => (open === index ? close() : setOpen(index))}
            onPointerEnter={() => {
              if (open !== null && open !== index) setOpen(index);
            }}
          >
            {menu.label}
          </button>
          {open === index && (
            <ul role="menu" className="menubar__dropdown" aria-label={menu.label}>
              {renderItems(menu.items, 1)}
            </ul>
          )}
        </div>
      ))}
    </nav>
  );
}
