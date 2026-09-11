fn activate() {
    notify("Container notifier is active");
}

fn on_event(event) {
    notify(`Docker event: ${event}`);
}
