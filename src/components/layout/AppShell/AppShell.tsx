import type { ReactNode } from "react";

interface AppShellProps {
  children: ReactNode;
}

export function AppShell({ children }: AppShellProps) {
  return (
    <div className="flex h-full min-h-[32rem] flex-col overflow-hidden bg-surface">
      <header className="shrink-0 border-b border-line bg-white/95">
        <div className="mx-auto flex h-[4.5rem] w-full max-w-[90rem] items-center justify-between px-6 lg:px-10">
          <div className="flex items-center gap-3">
            <div
              aria-hidden="true"
              className="grid size-8 place-items-center rounded-lg bg-brand-700 text-[0.6875rem] font-bold tracking-[0.08em] text-white"
            >
              MI
            </div>
            <div>
              <p className="text-[0.9375rem] font-semibold tracking-[-0.01em] text-ink-950">
                Mill-it Ops
              </p>
              <p className="mt-0.5 text-[0.6875rem] font-medium tracking-[0.04em] text-ink-500">
                Operations Intelligence
              </p>
            </div>
          </div>

          <div className="flex items-center gap-2 rounded-full border border-line bg-surface px-3 py-1.5">
            <span className="size-1.5 rounded-full bg-brand-600" aria-hidden="true" />
            <span className="text-xs font-medium text-ink-600">Local</span>
          </div>
        </div>
      </header>

      <main className="flex min-h-0 flex-1 flex-col">{children}</main>
    </div>
  );
}
