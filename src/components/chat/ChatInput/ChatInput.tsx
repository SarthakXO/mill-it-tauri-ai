import { useState, type FormEvent, type KeyboardEvent } from "react";

interface ChatInputProps {
  disabled?: boolean;
  thinkingEnabled: boolean;
  onThinkingChange: (enabled: boolean) => void;
  onSubmit: (message: string) => void;
}

export function ChatInput({
  disabled = false,
  thinkingEnabled,
  onThinkingChange,
  onSubmit,
}: ChatInputProps) {
  const [draft, setDraft] = useState("");
  const canSubmit = !disabled && draft.trim().length > 0;

  const submitDraft = () => {
    if (!canSubmit) return;

    onSubmit(draft.trim());
    setDraft("");
  };

  const handleSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    submitDraft();
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
      event.preventDefault();
      submitDraft();
    }
  };

  return (
    <div className="shrink-0 border-t border-line bg-white px-4 pb-3 pt-4 sm:px-6">
      <form className="mx-auto w-full max-w-3xl" onSubmit={handleSubmit}>
        <div className="mb-2.5 flex items-center justify-between gap-4 px-1">
          <div>
            <label htmlFor="thinking-mode" className="text-xs font-medium text-ink-800">
              Thinking mode
            </label>
            <p id="thinking-mode-help" className="mt-0.5 text-[0.6875rem] text-ink-500">
              {thinkingEnabled
                ? "Uses deeper reasoning. Responses may take longer."
                : "Faster responses"}
            </p>
          </div>
          <button
            id="thinking-mode"
            type="button"
            role="switch"
            aria-checked={thinkingEnabled}
            aria-describedby="thinking-mode-help"
            onClick={() => onThinkingChange(!thinkingEnabled)}
            className={`relative h-6 w-11 shrink-0 rounded-full transition focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand-600 ${
              thinkingEnabled ? "bg-brand-700" : "bg-[#cbd3ce]"
            }`}
          >
            <span className="sr-only">Toggle Thinking mode</span>
            <span
              aria-hidden="true"
              className={`absolute top-1 h-4 w-4 rounded-full bg-white shadow-sm transition-transform ${
                thinkingEnabled ? "translate-x-6" : "translate-x-1"
              }`}
            />
          </button>
        </div>
        <div className="flex items-end gap-3 rounded-xl border border-[#cfd9d2] bg-white p-2 shadow-[0_4px_18px_rgba(22,33,29,0.07)] transition focus-within:border-brand-600 focus-within:ring-3 focus-within:ring-brand-100">
          <label htmlFor="chat-input" className="sr-only">
            Ask Mill-it Ops a question
          </label>
          <textarea
            id="chat-input"
            value={draft}
            disabled={disabled}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={handleKeyDown}
            rows={1}
            placeholder="Ask about orders, products, customers or operations..."
            className="field-sizing-content min-h-11 max-h-36 flex-1 resize-none bg-transparent px-3 py-2.5 text-sm leading-6 text-ink-950 outline-none placeholder:text-ink-500 disabled:cursor-not-allowed disabled:text-ink-500"
          />
          <button
            type="submit"
            disabled={!canSubmit}
            className="mb-0.5 flex h-10 shrink-0 items-center gap-2 rounded-lg bg-brand-700 px-4 text-sm font-semibold text-white transition hover:bg-brand-600 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand-600 disabled:cursor-not-allowed disabled:bg-[#d9dfdb] disabled:text-[#89938e]"
          >
            Send
            <span aria-hidden="true" className="text-base leading-none">
              ↑
            </span>
          </button>
        </div>
        <p className="mt-2 text-center text-[0.6875rem] text-ink-500">
          Enter to send · Shift + Enter for a new line
        </p>
      </form>
    </div>
  );
}
