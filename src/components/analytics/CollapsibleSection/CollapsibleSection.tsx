import { useId, useState, type ReactNode } from "react";

interface CollapsibleSectionProps {
  title: string;
  subtitle?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  defaultExpanded?: boolean;
  className?: string;
}

export function CollapsibleSection({
  title,
  subtitle,
  actions,
  children,
  defaultExpanded = true,
  className = "",
}: CollapsibleSectionProps) {
  const [expanded, setExpanded] = useState(defaultExpanded);
  const contentId = useId();

  return (
    <section
      className={`card-shadow overflow-hidden rounded-xl border border-line bg-panel ${className}`}
    >
      <div className="flex min-h-16 flex-wrap items-center gap-3 border-b border-line px-4 py-3">
        <button
          type="button"
          className="group flex min-w-0 flex-1 items-center gap-3 text-left focus-visible:rounded-md focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand-600"
          aria-expanded={expanded}
          aria-controls={contentId}
          onClick={() => setExpanded((current) => !current)}
        >
          <span
            aria-hidden="true"
            className={`grid size-6 shrink-0 place-items-center rounded-md text-ink-500 transition group-hover:bg-brand-50 group-hover:text-brand-600 ${
              expanded ? "rotate-0" : "-rotate-90"
            }`}
          >
            <svg viewBox="0 0 20 20" fill="none" className="size-4">
              <path d="m6 8 4 4 4-4" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          </span>
          <span className="min-w-0">
            <span className="block truncate text-sm font-semibold text-ink-950">{title}</span>
            {subtitle && <span className="mt-0.5 block text-[0.6875rem] text-ink-500">{subtitle}</span>}
          </span>
        </button>
        {actions && <div className="ml-auto flex max-w-full flex-wrap items-center justify-end gap-2">{actions}</div>}
      </div>

      <div id={contentId} hidden={!expanded}>
        {children}
      </div>
    </section>
  );
}
