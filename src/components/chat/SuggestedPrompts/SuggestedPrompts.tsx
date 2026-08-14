interface SuggestedPromptsProps {
  prompts: readonly string[];
  onSelect: (prompt: string) => void;
}

export function SuggestedPrompts({ prompts, onSelect }: SuggestedPromptsProps) {
  return (
    <div className="grid w-full grid-cols-1 gap-2.5 sm:grid-cols-2">
      {prompts.map((prompt) => (
        <button
          key={prompt}
          type="button"
          onClick={() => onSelect(prompt)}
          className="card-shadow group flex min-h-20 items-start justify-between gap-4 rounded-xl border border-line bg-panel px-4 py-3.5 text-left text-[0.8125rem] font-medium leading-5 text-ink-800 transition hover:-translate-y-px hover:bg-brand-50 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand-600"
        >
          <span>{prompt}</span>
          <span
            aria-hidden="true"
            className="mt-0.5 text-base leading-none text-ink-500 transition group-hover:translate-x-0.5 group-hover:text-brand-700"
          >
            →
          </span>
        </button>
      ))}
    </div>
  );
}
