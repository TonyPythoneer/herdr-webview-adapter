// Browser URL prompt: follows the upstream prompt mod but returns "url",
// not the rename prompt's "name". The Rust command validates navigation.
import { View, div } from "gpui-kit";
import { v_flex, h_flex, InputState, Input } from "gpui-base";
import { run, props, closeOverlay, theme } from "herdr";

export default class BrowserUrl extends View {
  init(_props, cx) {
    this.value = props()?.value ?? "";
    this.input = InputState.new({ value: this.value });
    if (this.value) this.input.set_selected_range({ start: 0, end: this.value.length });
    this.input.on("change", (_e, cx) => { this.value = this.input.value(); cx.notify(); });
    this.input.on("submit", () => this.submit());
    this.input.focus();
    this.handle = cx.focus_handle();
  }
  submit() {
    const p = props();
    closeOverlay(p.overlay);
    run(p.command.name, { ...(p.command.args ?? {}), url: this.value });
  }
  key(e, cx) {
    if (e.key === "escape") { closeOverlay(props().overlay); cx.stop_propagation(); }
  }
  render() {
    const t = theme();
    const p = props() ?? {};
    const button = (id, text, primary, action) => h_flex().id(id).h(32).px(14).items_center().rounded(8)
      .border_1().border_color(primary ? t.solid : t.input).bg(primary ? t.solid : t.raised)
      .text_color(primary ? t.solid_foreground : t.foreground).text_size(12.5).font_medium().cursor_pointer()
      .hover(s => s.opacity(0.88)).on_click(action).child(text);
    return v_flex().id("browser-url").track_focus(this.handle).on_key_down((e, cx) => this.key(e, cx))
      .w_full().min_h(0).p(18).bg(t.popover).text_color(t.foreground)
      .child(div().text_size(15).font_semibold().child(p.title ?? "Navigate"))
      .child(div().pt(16).pb(7).text_size(11.5).text_color(t.dim).child("URL"))
      .child(Input.new(this.input).h(36).px(10).rounded(9).border_1().border_color(t.accent)
        .bg(t.background).text_size(12.8).text_color(t.foreground))
      .child(div().pt(6).text_size(11.5).text_color(t.dim).child(p.hint ?? "https://example.com"))
      .child(h_flex().pt(20).gap(8).justify_end()
        .child(button("bu-cancel", "Cancel", false, () => closeOverlay(p.overlay)))
        .child(button("bu-go", "Go", true, () => this.submit())));
  }
}
