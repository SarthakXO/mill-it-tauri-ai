import { SUGGESTED_PROMPTS } from "../../../constants/chat";
import { SuggestedPrompts } from "../SuggestedPrompts/SuggestedPrompts";

interface EmptyChatProps {
  onPromptSelect: (prompt: string) => void;
}

export function EmptyChat({ onPromptSelect }: EmptyChatProps) {
  return (
    <section className="mx-auto flex w-full max-w-2xl flex-col items-center px-6 text-center">
      <div
        aria-hidden="true"
        className="card-shadow mb-5 grid size-11 place-items-center rounded-xl border border-line bg-brand-50 text-brand-600"
      >
        <svg viewBox="0 0 24 24" fill="none" className="size-5" aria-hidden="true">
          <path
            d="M5 16.5V19l2.6-1.55c.45.1.92.15 1.4.15h5.5A4.5 4.5 0 0 0 19 13.1V8.5A4.5 4.5 0 0 0 14.5 4h-5A4.5 4.5 0 0 0 5 8.5v8Z"
            stroke="currentColor"
            strokeWidth="1.7"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
          <path d="M9 9h6M9 12.5h4" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" />
        </svg>
      </div>

      <p className="mb-2 text-xs font-semibold uppercase tracking-[0.14em] text-brand-700">
        Ask Mill-it
      </p>
      <h1 className="text-balance text-2xl font-semibold tracking-[-0.025em] text-ink-950 sm:text-[1.75rem]">
        What would you like to analyze?
      </h1>
      <p className="mt-3 max-w-lg text-sm leading-6 text-ink-600">
        Explore orders, products, customers, and operational trends from one focused workspace.
      </p>

      <div className="mt-8 w-full">
        <SuggestedPrompts prompts={SUGGESTED_PROMPTS} onSelect={onPromptSelect} />
      </div>
    </section>
  );
}
