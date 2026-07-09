import { useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/**
 * Assistant-message markdown: GFM (tables, strikethrough, task lists) plus a
 * copy button on fenced code blocks. Kept dependency-light — no syntax
 * highlighting; code renders monospace on a darker panel.
 */
export function Markdown({ children }: { children: string }) {
  return (
    <div className="md" style={{ fontSize: 13, lineHeight: 1.55 }}>
      <style>{mdCss}</style>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          pre: (props) => <CodeBlock {...props} />,
          a: ({ href, children: kids }) => (
            <a href={href} target="_blank" rel="noreferrer" style={{ color: "#38bdf8" }}>
              {kids}
            </a>
          ),
        }}
      >
        {children}
      </ReactMarkdown>
    </div>
  );
}

function CodeBlock(props: React.HTMLAttributes<HTMLPreElement>) {
  const [copied, setCopied] = useState(false);

  const copy = (e: React.MouseEvent<HTMLButtonElement>) => {
    const pre = e.currentTarget.parentElement?.querySelector("pre");
    const text = pre?.textContent ?? "";
    void navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    });
  };

  return (
    <div style={{ position: "relative", margin: "8px 0" }}>
      <button
        onClick={copy}
        style={{
          position: "absolute",
          top: 6,
          right: 6,
          border: "1px solid #1f2937",
          borderRadius: 6,
          background: "#131926",
          color: copied ? "#34d399" : "#94a3b8",
          fontSize: 11,
          padding: "2px 8px",
          cursor: "pointer",
        }}
      >
        {copied ? "Copied" : "Copy"}
      </button>
      <pre
        {...props}
        style={{
          background: "#05070c",
          border: "1px solid #1f2937",
          borderRadius: 8,
          padding: "10px 12px",
          overflowX: "auto",
          margin: 0,
          fontSize: 12,
          lineHeight: 1.5,
        }}
      />
    </div>
  );
}

const mdCss = `
.md > :first-child { margin-top: 0; }
.md > :last-child { margin-bottom: 0; }
.md p, .md ul, .md ol, .md blockquote, .md table { margin: 6px 0; }
.md h1, .md h2, .md h3, .md h4 { margin: 10px 0 4px; font-size: 14px; }
.md ul, .md ol { padding-left: 20px; }
.md li { margin: 2px 0; }
.md code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; }
.md :not(pre) > code { background: #1f2937; border-radius: 4px; padding: 1px 5px; }
.md blockquote { border-left: 3px solid #1f2937; padding-left: 10px; color: #94a3b8; }
.md table { border-collapse: collapse; display: block; overflow-x: auto; }
.md th, .md td { border: 1px solid #1f2937; padding: 4px 8px; text-align: left; }
.md th { background: #131926; }
.md hr { border: none; border-top: 1px solid #1f2937; margin: 10px 0; }
`;
