import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { MessageItem } from "../types";
import type { Components } from "react-markdown";

type MessageBubbleProps = {
  message: MessageItem;
};

const markdownComponents: Components = {
  pre({ children }) {
    return (
      <pre className="bg-[rgba(0,0,0,0.3)] rounded-lg p-3 overflow-x-auto my-2 text-sm">
        {children}
      </pre>
    );
  },
  code({ className, children, ...rest }) {
    const isInline = !className;
    if (isInline) {
      return (
        <code className="bg-[rgba(0,0,0,0.3)] px-1.5 py-0.5 rounded text-sm" {...rest}>
          {children}
        </code>
      );
    }
    return (
      <code className={className} {...rest}>
        {children}
      </code>
    );
  },
};

export function MessageBubble({ message }: MessageBubbleProps) {
  const isStreaming = message.id.startsWith("draft:");

  return (
    <article
      className={`bubble ${message.is_from_bot ? "bubble-bot" : "bubble-user"}`}
    >
      <div className="bubble-meta">
        <span>{message.sender_name}</span>
        <time>{new Date(message.timestamp).toLocaleTimeString()}</time>
      </div>
      {message.is_from_bot ? (
        <div className={isStreaming ? "streaming-cursor" : ""}>
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
            components={markdownComponents}
          >
            {message.content}
          </ReactMarkdown>
        </div>
      ) : (
        <pre>{message.content}</pre>
      )}
    </article>
  );
}
