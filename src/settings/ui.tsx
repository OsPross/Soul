import { useId, type ReactNode } from "react";
import { motion } from "framer-motion";
import { useAnim } from "../anim/AnimationProvider";

/** Settings control kit: glass cards, animated toggles / sliders / segmented controls. */

export function Card({
  title,
  description,
  action,
  children,
  className,
}: {
  title?: string;
  description?: ReactNode;
  action?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section
      className={`rounded-[20px] border border-white/[0.07] bg-white/[0.035] p-5 shadow-[inset_0_1px_0_rgba(255,255,255,0.04)] ${className ?? ""}`}
    >
      {(title || action) && (
        <header className="mb-4 flex items-start justify-between gap-3">
          <div>
            {title && <h3 className="text-[14px] font-semibold tracking-tight text-white">{title}</h3>}
            {description && <p className="mt-0.5 text-[12px] leading-relaxed text-[var(--soul-muted)]">{description}</p>}
          </div>
          {action}
        </header>
      )}
      <div className="flex flex-col gap-4">{children}</div>
    </section>
  );
}

export function Row({
  label,
  hint,
  children,
  stacked,
  disabled,
}: {
  label: ReactNode;
  hint?: ReactNode;
  children: ReactNode;
  stacked?: boolean;
  disabled?: boolean;
}) {
  return (
    <div
      className={`${stacked ? "flex flex-col gap-2" : "flex items-center justify-between gap-4"} ${
        disabled ? "pointer-events-none opacity-45" : ""
      } transition-opacity`}
    >
      <div className="min-w-0">
        <div className="text-[13px] text-white/90">{label}</div>
        {hint && <div className="mt-0.5 text-[11.5px] leading-relaxed text-[var(--soul-muted)]">{hint}</div>}
      </div>
      <div className={stacked ? "" : "shrink-0"}>{children}</div>
    </div>
  );
}

export function Toggle({
  checked,
  onChange,
  label,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label?: string;
}) {
  const anim = useAnim();
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      onClick={() => onChange(!checked)}
      className="relative h-[24px] w-[42px] shrink-0 rounded-full outline-none transition-colors duration-200 focus-visible:ring-2 focus-visible:ring-[var(--soul-accent)]/60"
      style={{ background: checked ? "var(--soul-accent)" : "rgba(255,255,255,0.14)" }}
    >
      <motion.span
        className="absolute top-[3px] h-[18px] w-[18px] rounded-full bg-white shadow-[0_1px_4px_rgba(0,0,0,0.45)]"
        initial={false}
        animate={{ left: checked ? 21 : 3 }}
        whileTap={anim.on("micro") ? { width: 22 } : undefined}
        transition={anim.spring("bouncy")}
      />
    </button>
  );
}

export function Slider({
  value,
  min,
  max,
  step = 0.01,
  onChange,
  format,
  leftLabel,
  rightLabel,
}: {
  value: number;
  min: number;
  max: number;
  step?: number;
  onChange: (v: number) => void;
  format?: (v: number) => string;
  leftLabel?: string;
  rightLabel?: string;
}) {
  const pct = ((value - min) / (max - min)) * 100;
  return (
    <div className="w-[240px]">
      <div className="flex items-center gap-3">
        <input
          type="range"
          min={min}
          max={max}
          step={step}
          value={value}
          onChange={(e) => onChange(Number(e.target.value))}
          className="soul-range flex-1"
          style={{ ["--fill" as string]: `${pct}%` }}
        />
        {format && <span className="w-12 text-right text-[12px] tabular-nums text-white/70">{format(value)}</span>}
      </div>
      {(leftLabel || rightLabel) && (
        <div className="mt-1 flex justify-between pr-[60px] text-[10.5px] text-white/35">
          <span>{leftLabel}</span>
          <span>{rightLabel}</span>
        </div>
      )}
    </div>
  );
}

export function Segmented<T extends string | number>({
  value,
  options,
  onChange,
  size = "md",
}: {
  value: T;
  options: { value: T; label: ReactNode }[];
  onChange: (v: T) => void;
  size?: "sm" | "md";
}) {
  const anim = useAnim();
  const id = useId();
  return (
    <div className="inline-flex rounded-full bg-white/[0.06] p-[3px]" role="radiogroup">
      {options.map((o) => {
        const active = o.value === value;
        return (
          <button
            key={String(o.value)}
            type="button"
            role="radio"
            aria-checked={active}
            onClick={() => onChange(o.value)}
            className={`relative rounded-full ${size === "sm" ? "px-2.5 py-1 text-[11.5px]" : "px-3.5 py-1.5 text-[12.5px]"} transition-colors ${
              active ? "text-white" : "text-white/50 hover:text-white/80"
            }`}
          >
            {active && (
              <motion.span
                layoutId={anim.on("tabs") ? `seg-${id}` : undefined}
                transition={anim.spring("ui")}
                className="absolute inset-0 rounded-full bg-white/[0.14] shadow-[0_1px_3px_rgba(0,0,0,0.3)]"
              />
            )}
            <span className="relative whitespace-nowrap">{o.label}</span>
          </button>
        );
      })}
    </div>
  );
}

export function Select({
  value,
  onChange,
  children,
  disabled,
  className,
}: {
  value: string;
  onChange: (v: string) => void;
  children: ReactNode;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <select
      value={value}
      disabled={disabled}
      onChange={(e) => onChange(e.target.value)}
      className={`soul-input px-3 py-2 text-[13px] disabled:opacity-45 ${className ?? "w-[280px]"}`}
    >
      {children}
    </select>
  );
}

export function Button({
  children,
  onClick,
  variant = "ghost",
  disabled,
  type = "button",
  className,
}: {
  children: ReactNode;
  onClick?: () => void;
  variant?: "ghost" | "primary" | "danger";
  disabled?: boolean;
  type?: "button" | "submit";
  className?: string;
}) {
  const anim = useAnim();
  const micro = anim.on("micro");
  const styles =
    variant === "primary"
      ? "bg-[var(--soul-accent)] text-[#0b0d0c] font-semibold shadow-[0_4px_16px_-6px_var(--soul-accent)]"
      : variant === "danger"
        ? "bg-[var(--soul-danger)]/15 text-[var(--soul-danger)] hover:bg-[var(--soul-danger)]/25"
        : "bg-white/[0.07] text-white/85 hover:bg-white/[0.12]";
  return (
    <motion.button
      type={type}
      onClick={onClick}
      disabled={disabled}
      whileHover={micro && !disabled ? { y: -1 } : undefined}
      whileTap={micro && !disabled ? { scale: 0.97 } : undefined}
      transition={anim.spring("ui")}
      className={`rounded-full px-4 py-2 text-[12.5px] transition-colors disabled:opacity-40 ${styles} ${className ?? ""}`}
    >
      {children}
    </motion.button>
  );
}

export function Callout({ tone = "info", children }: { tone?: "info" | "warn" | "danger"; children: ReactNode }) {
  const c =
    tone === "warn"
      ? "border-amber-300/25 bg-amber-300/[0.08] text-amber-100"
      : tone === "danger"
        ? "border-[var(--soul-danger)]/30 bg-[var(--soul-danger)]/10 text-[var(--soul-danger)]"
        : "border-white/10 bg-white/[0.04] text-white/75";
  return <div className={`rounded-2xl border px-3.5 py-2.5 text-[12px] leading-relaxed ${c}`}>{children}</div>;
}

/** A key rendered like a physical keycap. */
export function Keycap({ children, active }: { children: ReactNode; active?: boolean }) {
  return (
    <kbd
      className={`inline-flex min-w-[30px] items-center justify-center rounded-[9px] border px-2.5 py-1 font-sans text-[12px] font-medium transition-colors ${
        active ? "border-[var(--soul-accent)]/60 text-[var(--soul-accent)]" : "border-white/15 text-white/90"
      }`}
      style={{
        background: "linear-gradient(180deg, rgba(255,255,255,0.12), rgba(255,255,255,0.04))",
        boxShadow: "0 2px 0 rgba(0,0,0,0.55), inset 0 1px 0 rgba(255,255,255,0.12)",
      }}
    >
      {children}
    </kbd>
  );
}

export function SectionReset({ onClick, label }: { onClick: () => void; label: string }) {
  return (
    <button type="button" onClick={onClick} className="rounded-full px-2.5 py-1 text-[11px] text-white/40 hover:bg-white/5 hover:text-white/80">
      ↺ {label}
    </button>
  );
}
