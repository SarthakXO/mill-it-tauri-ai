import type { ChatMessage as ChatMessageType } from "../../../types/chat";
import { AnalyticsResponse } from "../../analytics/AnalyticsResponse/AnalyticsResponse";

interface ChatMessageProps {
  message: ChatMessageType;
}

export function ChatMessage({ message }: ChatMessageProps) {
  const isUser = message.role === "user";
  const isError = message.content.type === "text" && message.content.tone === "error";

  return (
    <article className={isUser ? "bg-panel" : "bg-surface"}>
      <div
        className={`mx-auto flex w-full gap-4 px-6 py-6 sm:px-8 ${
          isUser ? "max-w-3xl" : "max-w-5xl"
        }`}
      >
        <div
          aria-hidden="true"
          className={
            isUser
              ? "grid size-7 shrink-0 place-items-center rounded-md bg-ink-800 text-[0.625rem] font-bold text-white"
              : "grid size-7 shrink-0 place-items-center rounded-md bg-brand-100 text-[0.625rem] font-bold text-brand-700"
          }
        >
          {isUser ? "YOU" : "MI"}
        </div>

        <div className="min-w-0 flex-1 pt-0.5">
          <p className="mb-2 text-xs font-semibold text-ink-500">
            {isUser ? "You" : "Mill-it Ops"}
          </p>
          {message.content.type === "analytics" ? (
            <AnalyticsResponse response={message.content.response} />
          ) : (
            <div
              className={`whitespace-pre-wrap break-words text-[0.9375rem] leading-6 ${
                isError ? "text-danger" : "text-ink-800"
              }`}
            >
              {message.content.text}
            </div>
          )}
        </div>
      </div>
    </article>
  );
}
