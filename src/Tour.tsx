import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { cardPlacement, TOUR_STEPS, tourStep } from "./tour";

type Props = {
  /** Called when the tour ends, by Done, Skip, or Esc. */
  onClose: () => void;
};

type Box = { top: number; left: number; width: number; height: number };

/**
 * Help > Welcome Tour: one card per `TOUR_STEPS` entry, a ring around
 * the step's target, Back / Next / Done, Skip, and Esc. The target's box
 * is read from the page on every step and on resize.
 */
export default function Tour({ onClose }: Props) {
  const [index, setIndex] = useState(0);
  const [box, setBox] = useState<Box | null>(null);
  const [placement, setPlacement] = useState({ top: 0, left: 0 });
  const card = useRef<HTMLDivElement>(null);
  const step = TOUR_STEPS[index];

  const measure = useCallback(() => {
    const target = step.target ? document.querySelector(step.target) : null;
    const rect = target?.getBoundingClientRect();
    // The visible part of the target only: an element wider than the
    // window would otherwise centre the card off-screen.
    const next = (() => {
      if (!rect || rect.width <= 0) return null;
      const left = Math.max(rect.left, 0);
      const top = Math.max(rect.top, 0);
      const right = Math.min(rect.right, window.innerWidth);
      const bottom = Math.min(rect.bottom, window.innerHeight);
      return right > left && bottom > top
        ? { top, left, width: right - left, height: bottom - top }
        : null;
    })();
    setBox(next);
    const size = card.current
      ? { width: card.current.offsetWidth, height: card.current.offsetHeight }
      : { width: 360, height: 180 };
    setPlacement(
      cardPlacement(next, { width: window.innerWidth, height: window.innerHeight }, size),
    );
  }, [step.target]);

  useLayoutEffect(measure, [measure]);
  useEffect(() => {
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [measure]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
      if (event.key === "ArrowRight") setIndex((i) => tourStep(i, 1) ?? i);
      if (event.key === "ArrowLeft") setIndex((i) => tourStep(i, -1) ?? i);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const last = tourStep(index, 1) === null;
  return (
    <div className="tour" role="dialog" aria-label="Welcome tour" aria-modal="true">
      {box && (
        <div
          className="tour__ring"
          style={{
            top: box.top - 4,
            left: box.left - 4,
            width: box.width + 8,
            height: box.height + 8,
          }}
          aria-hidden="true"
        />
      )}
      <div className="tour__card" ref={card} style={{ top: placement.top, left: placement.left }}>
        <p className="tour__count">
          {index + 1} of {TOUR_STEPS.length}
        </p>
        <h2 className="tour__title">{step.title}</h2>
        <p className="tour__body">{step.body}</p>
        <div className="tour__actions">
          <button className="button button--quiet" onClick={onClose}>
            Skip
          </button>
          <span className="tour__spacer" />
          <button
            className="button button--quiet"
            onClick={() => setIndex((i) => tourStep(i, -1) ?? i)}
            disabled={index === 0}
          >
            Back
          </button>
          <button
            className="button"
            onClick={() => (last ? onClose() : setIndex((i) => tourStep(i, 1) ?? i))}
            autoFocus
          >
            {last ? "Done" : "Next"}
          </button>
        </div>
      </div>
    </div>
  );
}
