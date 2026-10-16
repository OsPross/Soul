import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { AnimationProvider } from "./anim/AnimationProvider";
import Highlight from "./agent/Highlight";
import AskView from "./ask/AskView";
import Island from "./island/Island";
import Overlay from "./overlay/Overlay";
import Settings from "./settings/Settings";

export default function App() {
  const [label, setLabel] = useState<string | null>(null);

  useEffect(() => {
    setLabel(getCurrentWindow().label);
  }, []);

  if (!label) {
    return null;
  }

  // Every window shares the central animation / appearance config.
  return (
    <AnimationProvider>
      {label === "settings" ? (
        <Settings />
      ) : label === "ask" ? (
        <AskView />
      ) : label === "island" ? (
        <Island />
      ) : label === "agent-highlight" ? (
        <Highlight />
      ) : (
        <Overlay />
      )}
    </AnimationProvider>
  );
}
