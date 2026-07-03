import { useState, type ComponentProps } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

const FOLD_THRESHOLD = 20;

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

type PreProps = Omit<ComponentProps<"pre">, "node">;

function CodeBlockPre({ node: _, ...props }: PreProps & { node?: unknown }) {
  const [copied, setCopied] = useState(false);
  const [expanded, setExpanded] = useState(false);

  const rawText = extractText(props.children).replace(/^\n+|\n+$/g, "");
  const lines = rawText.split("\n");
  const isLong = lines.length > FOLD_THRESHOLD;

  const handleCopy = () => {
    navigator.clipboard.writeText(rawText).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  };

  const displayChildren = isLong && !expanded
    ? lines.slice(0, FOLD_THRESHOLD).join("\n")
    : rawText;

  return (
    <pre {...props}>
      <button
        type="button"
        className="code-block-copy"
        onClick={handleCopy}
      >
        {copied ? "Copied" : "Copy"}
      </button>
      <code>{displayChildren}</code>
      {isLong && (
        <button
          type="button"
          className="code-block-fold"
          onClick={() => setExpanded((e) => !e)}
        >
          {expanded
            ? "Collapse"
            : `Show all (${lines.length} lines)`}
        </button>
      )}
    </pre>
  );
}

function extractText(node: unknown): string {
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(extractText).join("");
  if (node && typeof node === "object" && "props" in node) {
    const props = (node as { props: { children?: unknown } }).props;
    if (props.children == null) return "";
    return extractText(props.children);
  }
  return "";
}
