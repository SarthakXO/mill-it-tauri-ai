import { useEffect, useRef } from "react";
import type { ChatMessage as ChatMessageType } from "../../../types/chat";
import { ChatLoading } from "../ChatLoading/ChatLoading";
import { ChatMessage } from "../ChatMessage/ChatMessage";
import { EmptyChat } from "../EmptyChat/EmptyChat";

interface ChatWindowProps {
  messages: ChatMessageType[];
  isGenerating: boolean;
  onPromptSelect: (prompt: string) => void;
}

export function ChatWindow({ messages, isGenerating, onPromptSelect }: ChatWindowProps) {
  const endOfMessagesRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (messages.length > 0) {
      endOfMessagesRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
    }
  }, [messages, isGenerating]);

  if (messages.length === 0) {
    return (
      <div className="chat-scrollbar flex min-h-0 flex-1 items-center overflow-y-auto py-10 lg:py-14">
        <EmptyChat onPromptSelect={onPromptSelect} />
      </div>
    );
  }

  return (
    <div
      className="chat-scrollbar min-h-0 flex-1 overflow-y-auto"
      role="log"
      aria-live="polite"
      aria-label="Conversation"
    >
      {messages.map((message) => (
        <ChatMessage key={message.id} message={message} />
      ))}
      {isGenerating && <ChatLoading />}
      <div ref={endOfMessagesRef} />
    </div>
  );
}
