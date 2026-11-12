import { useEffect, useRef, useState } from "react";
import QRCode from "qrcode";
import type { PairInfo } from "./api";

export function Pairing({ pair }: { pair: PairInfo }) {
  const [now, setNow] = useState(Date.now());
  const [image, setImage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const section = useRef<HTMLElement>(null);
  const remaining = Math.max(0, Math.ceil((pair.expires_at - now) / 1000));

  useEffect(() => {
    let cancelled = false;
    setNow(Date.now());
    setImage(null);
    setError(null);
    if (pair.expires_at > Date.now()) {
      void QRCode.toDataURL(pair.qr, { width: 256, margin: 2, errorCorrectionLevel: "M" })
        .then((url) => { if (!cancelled) setImage(url); })
        .catch((reason: unknown) => { if (!cancelled) setError(`二维码生成失败：${String(reason)}`); });
    }
    section.current?.focus();
    section.current?.scrollIntoView?.({ block: "nearest" });
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => { cancelled = true; clearInterval(timer); };
  }, [pair]);

  return (
    <section aria-labelledby="pair-title" ref={section} tabIndex={-1} className="pairing">
      <h3 id="pair-title">手机配对</h3>
      {remaining > 0 ? <>
        <p className="muted">在手机上扫描二维码。</p>
        {image ? <img src={image} alt="配对二维码" width={256} height={256} /> : !error && <p className="muted">正在生成二维码…</p>}
        {error && <p role="alert" className="alert">{error}</p>}
        <p role="timer" aria-label="配对倒计时" className={remaining <= 60 ? "timer soon" : "timer"}>剩余 {Math.floor(remaining / 60)} 分 {remaining % 60} 秒</p>
      </> : <p role="status" className="muted">二维码已过期，请重新配对。</p>}
    </section>
  );
}
