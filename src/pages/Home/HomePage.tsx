import { useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ChatInput } from "../../components/chat/ChatInput/ChatInput";
import { ChatWindow } from "../../components/chat/ChatWindow/ChatWindow";
import { AppShell } from "../../components/layout/AppShell/AppShell";
import type { AgentResponse, ChatMessage } from "../../types/chat";

function createTextMessage(
  role: ChatMessage["role"],
  text: string,
  tone: "default" | "error" = "default",
): ChatMessage {
  return {
    id: crypto.randomUUID(),
    role,
    content: { type: "text", text, tone },
  };
}

function createAgentMessage(response: AgentResponse): ChatMessage {
  return {
    id: crypto.randomUUID(),
    role: "assistant",
    content: { type: "analytics", response },
  };
}

export function HomePage() {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [isGenerating, setIsGenerating] = useState(false);
  const requestInFlight = useRef(false);
  const [thinkingEnabled, setThinkingEnabled] = useState(
    () => localStorage.getItem("millit-thinking-enabled") === "true",
  );
  const thinkingEnabledRef = useRef(thinkingEnabled);

  const toggleThinking = (enabled: boolean) => {
    thinkingEnabledRef.current = enabled;
    setThinkingEnabled(enabled);
    localStorage.setItem("millit-thinking-enabled", String(enabled));
  };

  const submitMessage = async (text: string) => {
    if (requestInFlight.current) return;

    requestInFlight.current = true;
    const userMessage = createTextMessage("user", text);
    const conversation = [...messages, userMessage];
    setMessages(conversation);
    setIsGenerating(true);

    try {
      const response = await invoke<AgentResponse>("chat_with_ollama", {
        thinkingEnabled: thinkingEnabledRef.current,
        messages: conversation
          .filter(
            (message) => message.content.type !== "text" || message.content.tone !== "error",
          )
          .map((message) => ({
            role: message.role,
            content:
              message.content.type === "analytics"
                ? message.content.response.answer
                : message.content.text,
          })),
      });
      setMessages((currentMessages) => [
        ...currentMessages,
        createAgentMessage(response),
      ]);
    } catch (error) {
      const message =
        typeof error === "string"
          ? error
          : "Unable to reach the local AI model. Check that Ollama is running.";
      setMessages((currentMessages) => [
        ...currentMessages,
        createTextMessage("assistant", message, "error"),
      ]);
    } finally {
      requestInFlight.current = false;
      setIsGenerating(false);
    }
  };

  return (
    <AppShell>
      <ChatWindow
        messages={messages}
        isGenerating={isGenerating}
        onPromptSelect={(prompt) => void submitMessage(prompt)}
      />
      <ChatInput
        disabled={isGenerating}
        thinkingEnabled={thinkingEnabled}
        onThinkingChange={toggleThinking}
        onSubmit={(message) => void submitMessage(message)}
      />
    </AppShell>
  );
}
