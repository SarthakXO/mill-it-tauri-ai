export function ChatLoading() {
  return (
    <div className="bg-surface" role="status" aria-label="Mill-it Ops is thinking">
      <div className="mx-auto flex w-full max-w-3xl gap-4 px-6 py-6 sm:px-8">
        <div
          aria-hidden="true"
          className="grid size-7 shrink-0 place-items-center rounded-md bg-brand-100 text-[0.625rem] font-bold text-brand-700"
        >
          MI
        </div>
        <div className="min-w-0 flex-1 pt-0.5">
          <p className="mb-2 text-xs font-semibold text-ink-500">Mill-it Ops</p>
          <div className="flex items-center gap-2 text-sm text-ink-600">
            <span className="size-1.5 animate-pulse rounded-full bg-brand-600" aria-hidden="true" />
            Thinking…
          </div>
        </div>
      </div>
    </div>
  );
}
