import type { ReactNode } from "react";
import { useTheme } from "../../theme/ThemeProvider";

interface AppShellProps {
  children: ReactNode;
}

export function AppShell({ children }: AppShellProps) {
  const { theme, toggleTheme } = useTheme();

  return (
    <div className="flex h-full min-h-[32rem] flex-col overflow-hidden bg-surface">
      <header className="shrink-0 border-b border-line bg-panel/95">
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

          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={toggleTheme}
              className="flex h-9 items-center gap-2 rounded-full border border-line bg-panel-elevated px-3 text-xs font-semibold text-ink-600 transition hover:bg-brand-50 hover:text-ink-950 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand-600"
              aria-label={`Switch to ${theme === "dark" ? "light" : "dark"} mode`}
              title={`Current theme: ${theme === "dark" ? "Dark" : "Light"}. Switch to ${theme === "dark" ? "Light" : "Dark"} mode.`}
            >
              {theme === "dark" ? (
                <svg viewBox="0 0 20 20" fill="none" className="size-4" aria-hidden="true">
                  <circle cx="10" cy="10" r="3" stroke="currentColor" strokeWidth="1.5" />
                  <path d="M10 2v1.5M10 16.5V18M2 10h1.5M16.5 10H18M4.35 4.35l1.05 1.05M14.6 14.6l1.05 1.05M15.65 4.35 14.6 5.4M5.4 14.6l-1.05 1.05" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
                </svg>
              ) : (
                <svg viewBox="0 0 20 20" fill="none" className="size-4" aria-hidden="true">
                  <path d="M16.5 12.3A6.7 6.7 0 0 1 7.7 3.5 6.75 6.75 0 1 0 16.5 12.3Z" stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round" />
                </svg>
              )}
              <span className="hidden sm:inline">{theme === "dark" ? "Dark" : "Light"}</span>
            </button>
            <div className="flex items-center gap-2 rounded-full border border-line bg-panel-elevated px-3 py-1.5">
              <span className="size-1.5 rounded-full bg-brand-600" aria-hidden="true" />
              <span className="text-xs font-medium text-ink-600">Local</span>
            </div>
          </div>
        </div>
      </header>

      <main className="flex min-h-0 flex-1 flex-col">{children}</main>
    </div>
  );
}
