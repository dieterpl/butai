// The client's entry point.
//
// `bootTheme()` runs before `createRoot`, so the palette is on <html> before the
// first frame that has anything in it. Doing it inside a component would paint
// one frame in the default colours and then correct itself, which is the flash
// every themed app has to design around and this one does not have to.

import { createRoot } from "react-dom/client";
import { App } from "./App.tsx";
import { Shell } from "./app/Shell.tsx";
import { bootTheme } from "./theme.ts";
import "./styles.css";

bootTheme();

const root = document.getElementById("root");
if (!root) throw new Error("no #root — index.html and this file disagree");
// `?kit` is the gallery — every component on one page, in any palette. It is
// how the audit that started this rewrite was possible, so it stays reachable
// rather than being deleted once the app exists.
const kit = new URLSearchParams(location.search).has("kit");
createRoot(root).render(kit ? <App /> : <Shell />);
