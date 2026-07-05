import { useState, type ComponentProps } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import "highlight.js/styles/github-dark.css";

const FOLD_THRESHOLD = 20;

export interface MarkdownRendererProps {
  content: string;
}

export function MarkdownRenderer({ content }: MarkdownRendererProps) {
  return (
    <div className="markdown-content">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeHighlight]}
        components={{ pre: CodeBlockPre }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}

type PreProps = Omit<ComponentProps<"pre">, "node">;

function CodeBlockPre({ node: _, children, ...props }: PreProps & { node?: unknown }) {
  const [copied, setCopied] = useState(false);
  const [expanded, setExpanded] = useState(false);

  const rawText = extractText(children).replace(/^\n+|\n+$/g, "");
  const lineCount = rawText.split("\n").length;
  const isLong = lineCount > FOLD_THRESHOLD;
  const collapsed = isLong && !expanded;

  const handleCopy = () => {
    navigator.clipboard.writeText(rawText).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  };

  return (
    <pre {...props} className={collapsed ? "code-block-collapsed" : undefined}>
      <button
        type="button"
        className="code-block-copy"
        onClick={handleCopy}
      >
        {copied ? "Copied" : "Copy"}
      </button>
      {children}
      {isLong && (
        <button
          type="button"
          className="code-block-fold"
          onClick={() => setExpanded((e) => !e)}
        >
          {expanded ? "Collapse" : `Show all (${lineCount} lines)`}
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
