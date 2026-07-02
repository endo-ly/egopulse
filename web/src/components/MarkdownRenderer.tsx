import { useState, type ComponentProps } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

export interface MarkdownRendererProps {
  content: string;
}

export function MarkdownRenderer({ content }: MarkdownRendererProps) {
  return (
    <div className="markdown-content">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{ pre: CodeBlockPre }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}

type PreProps = ComponentProps<"pre">;

function CodeBlockPre(props: PreProps) {
  const [copied, setCopied] = useState(false);

  const handleCopy = () => {
    const text = extractText(props.children);
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  };

  return (
    <pre {...props}>
      <button
        type="button"
        className="code-block-copy"
        onClick={handleCopy}
      >
        {copied ? "Copied" : "Copy"}
      </button>
      {props.children}
    </pre>
  );
}

function extractText(node: unknown): string {
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(extractText).join("");
  if (node && typeof node === "object" && "props" in node) {
    const props = (node as { props: { children?: unknown } }).props;
    return extractText(props.children);
  }
  return "";
}
