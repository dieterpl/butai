// A real daemon, a real pane, real bytes on a real canvas.
//
// The pixel differential proves the port draws what the old renderer drew. It
// cannot prove the socket, the framing, the attach handshake and the resize
// negotiation still line up, because it never opens one — it hand-feeds frames.
// This mounts `<Stage>` against a live pane and lets the daemon drive.
import { createRoot } from "react-dom/client";
import { Stage } from "../../src/stage/Stage.tsx";

const pane = new URLSearchParams(location.search).get("pane");

function Live() {
  return (
    <div style={{ width: 900, height: 400 }}>
      <Stage
        pane={pane}
        theme={{ fg: "#d7dde5", bg: "#0e1116" }}
        fontPx={15}
        onDaemonVersion={(info) => {
          (window as unknown as { __version: unknown }).__version = info;
        }}
        onPaneRefused={(info) => {
          (window as unknown as { __refused: unknown }).__refused = info;
        }}
      />
    </div>
  );
}
createRoot(document.getElementById("root")!).render(<Live />);
