fn activate() {
    containers_read();
    notify("Requested a fresh container summary");
}

fn on_event(event) {
    if event == "health_status" {
        containers_read();
    }
}

fn restart_container(container_id) {
    containers_lifecycle(container_id, "restart");
}
