import { useEffect, useId, useRef, type ReactNode, type RefObject } from "react";

export type ModalFrameIds = {
  titleId: string;
  descriptionId: string;
};

export type ModalFrameProps = {
  children: (ids: ModalFrameIds) => ReactNode;
  onDismiss?: () => void;
  dismissible?: boolean;
  className?: string;
  initialFocusRef?: RefObject<HTMLElement | null>;
};

const FOCUSABLE_SELECTOR =
  "button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])";

/**
 * Shared modal shell for confirmation, error and progress surfaces.
 *
 * The caller owns the title/description markup and can nominate a safe
 * initial focus target. The shell owns focus containment and the background
 * inert boundary so each modal cannot accidentally leave the page interactive.
 */
export function ModalFrame({
  children,
  onDismiss,
  dismissible = true,
  className = "",
  initialFocusRef,
}: ModalFrameProps) {
  const veilRef = useRef<HTMLDivElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const onDismissRef = useRef(onDismiss);
  const titleId = useId();
  const descriptionId = useId();
  onDismissRef.current = onDismiss;

  useEffect(() => {
    const veil = veilRef.current;
    const dialog = dialogRef.current;
    previousFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;

    const parent = veil?.parentElement;
    const siblings = parent
      ? Array.from(parent.children).filter((element): element is HTMLElement => element !== veil)
      : [];
    const siblingInertState = siblings.map((element) => ({
      element,
      wasInert: element.hasAttribute("inert"),
    }));
    siblings.forEach((element) => element.setAttribute("inert", ""));

    const focusInitial = () => {
      const nominated = initialFocusRef?.current;
      const target = nominated && !nominated.hasAttribute("disabled")
        ? nominated
        : dialog?.querySelector<HTMLElement>("[data-autofocus]:not([disabled])") ??
          dialog?.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
      (target ?? dialog)?.focus({ preventScroll: true });
    };
    focusInitial();

    const visibleFocusable = () =>
      Array.from(dialog?.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR) ?? []).filter(
        (element) => !element.hasAttribute("hidden") && element.getClientRects().length > 0,
      );

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        // Consume Escape even for non-dismissible progress states so it cannot
        // reach a page-level handler and mutate the underlying workflow.
        event.preventDefault();
        event.stopPropagation();
        if (dismissible) onDismissRef.current?.();
        return;
      }
      if (event.key !== "Tab" || !dialog) return;

      const focusable = visibleFocusable();
      if (focusable.length === 0) {
        event.preventDefault();
        dialog.focus({ preventScroll: true });
        return;
      }

      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (!first || !last) return;
      if (!dialog.contains(document.activeElement)) {
        event.preventDefault();
        (event.shiftKey ? last : first).focus({ preventScroll: true });
      } else if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus({ preventScroll: true });
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus({ preventScroll: true });
      }
    };

    document.addEventListener("keydown", handleKeyDown, true);
    return () => {
      document.removeEventListener("keydown", handleKeyDown, true);
      siblingInertState.forEach(({ element, wasInert }) => {
        if (!wasInert) element.removeAttribute("inert");
      });
      restoreFocus(previousFocusRef.current);
    };
  }, [dismissible, initialFocusRef]);

  return (
    <div ref={veilRef} className="modal-veil">
      <div
        ref={dialogRef}
        className={`modal ${className}`.trim()}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={descriptionId}
        tabIndex={-1}
      >
        {children({ titleId, descriptionId })}
      </div>
    </div>
  );
}

function restoreFocus(previous: HTMLElement | null) {
  if (previous?.isConnected && !previous.hasAttribute("disabled")) {
    previous.focus({ preventScroll: true });
    return;
  }

  const fallback = document.querySelector<HTMLElement>(
    "[data-focus-fallback], main h1, main .page-title, h1, .page-title",
  );
  const target = fallback ?? document.body;
  const hadTabIndex = target.getAttribute("tabindex");
  if (hadTabIndex === null) target.setAttribute("tabindex", "-1");
  target.focus({ preventScroll: true });
}
