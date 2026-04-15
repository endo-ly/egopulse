type StatusBadgeProps = {
  tone: "idle" | "ok" | "error";
  text: string;
};

export function StatusBadge({ tone, text }: StatusBadgeProps) {
  return (
    <div
      className={`rounded-full px-3.5 py-2.5 text-sm bg-panel ${
        tone === "ok"
          ? "text-[#5ceaff]"
          : tone === "error"
            ? "text-[#fecaca]"
            : "text-muted"
      }`}
    >
      {text}
    </div>
  );
}
