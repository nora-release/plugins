use super::*;
use serde_json::json;
fn text(reply: &Value, path: &str) -> String {
    read(reply, path)
        .unwrap()
        .iter()
        .map(|part| {
            if part.label.is_empty() {
                part.text.clone()
            } else {
                format!("{} {}", part.label, part.text)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}
#[test]
fn magpie_scalar_arithmetic_and_multi_amount_cases() {
    let reply = json!({"code":true,"credits":{"monthlyCredits":42},"data":{"total_available":2500000,"name":"x","list":[{"left":"7.5"}]}});
    for (path, want) in [
        ("data.total_available", "2500000.00"),
        ("$ data.total_available / 500000", "$5.00"),
        ("¥data.list.0.left", "¥7.50"),
        ("data.name", "x"),
        (" data.total_available/1000000 ", "2.50"),
        ("(1-credits.monthlyCredits/70)%", "40%"),
        (
            "$ (data.total_available - data.list.0.left * 100000) / 500000",
            "$3.50",
        ),
        ("credits.monthlyCredits * 2 + 1", "85.00"),
        ("-credits.monthlyCredits", "-42.00"),
        ("credits.monthlyCredits / 70 %", "60%"),
    ] {
        assert_eq!(text(&reply, path), want, "{path}");
    }
    let cc = json!({"credits":{"monthlyCredits":70.0},"windowLimits":{"fiveHour":{"used":0,"cap":14},"weekly":{"used":1.19,"cap":35}}});
    for(path,want)in[("5h: windowLimits.fiveHour.used / windowLimits.fiveHour.cap %; week: windowLimits.weekly.used/windowLimits.weekly.cap %; $credits.monthlyCredits","5h 0% · week 3.4% · $70.00"),("credits.monthlyCredits;","70.00"),(" left : $credits.monthlyCredits","left $70.00")]{assert_eq!(text(&cc,path),want);}
    for path in [
        ";",
        "5h: windowLimits.hour.used; week: windowLimits.weekly.used / 0",
        "week:",
    ] {
        assert!(read(&cc, path).is_err(), "{path}");
    }
    for path in [
        "",
        "data.missing",
        "data.list.3.left",
        "data.total_available / zero",
        "data.list",
        "data.name + 1",
        "(data.total_available",
        "data.total_available / 0",
        "data.total_available 2",
        "%",
        "data.total_available%%",
    ] {
        assert!(read(&reply, path).is_err(), "{path}");
    }
}
#[test]
fn magpie_lists_filters_aggregates_and_partial_failures() {
    let one = json!({"data":{"subscriptions":[{"subscription":{"status":"active","amount_total":50000000,"amount_used":31593277,"weekly_amount_total":0,"weekly_amount_used":0}}],"all_subscriptions":[{"subscription":{"status":"expired","...":"..."}}]}});
    let two = json!({"data":{"subscriptions":[{"subscription":{"status":"active","amount_total":50000000,"amount_used":31593277,"weekly_amount_total":0,"weekly_amount_used":0}},{"subscription":{"status":"active","amount_total":"10000000","amount_used":5000000,"weekly_amount_total":4000000,"weekly_amount_used":1000000}},{"subscription":{"status":"expired","amount_total":90000000,"amount_used":90000000,"weekly_amount_total":1000000,"weekly_amount_used":1000000}}]}});
    let used="sum(data.subscriptions[subscription.status=active].subscription.amount_used) / sum(data.subscriptions[subscription.status=active].subscription.amount_total) %";
    let week="max(data.subscriptions[subscription.status=active].subscription.weekly_amount_used / data.subscriptions[subscription.status=active].subscription.weekly_amount_total) %";
    for(reply,path,want)in[(&one,used,"63.2%"),(&two,used,"61%"),(&one,"$ sum(data.subscriptions.*.subscription.amount_used) / 500000","$63.19"),(&two,"max(data.subscriptions.*.subscription.amount_used)","90000000.00"),(&two,"min(data.subscriptions.*.subscription.amount_used)","5000000.00"),(&two,"count(data.subscriptions[subscription.status!=active])","1.00"),(&one,"sum(data.all_subscriptions.*.subscription.amount_used)","0.00"),(&two,week,"25%"),(&two,"count(data.subscriptions.*.subscription[status='active'])","2.00"),(&two,"sum(data.subscriptions.*.subscription[status=active].amount_total) / 1000000","60.00"),(&two,"max(data.subscriptions.*.subscription.weekly_amount_used / data.subscriptions.*.subscription.weekly_amount_total) %","100%"),(&two,"sum(data.subscriptions[subscription.status=active].subscription.amount_used) * 2 / 1000000","73.19")]{assert_eq!(text(reply,path),want,"{path}");}
    assert_eq!(
        text(&json!([{"left":1},{"left":2.5}]), "sum(*.left)"),
        "3.50"
    );
    assert_eq!(
        text(&json!({"a":{"x":{"n":1},"y":{"n":4}}}), "max(a.*.n)"),
        "4.00"
    );
    assert_eq!(
        text(&one, &format!("used: {used}; week: {week}")),
        "used 63.2% · week —"
    );
    assert_eq!(text(&one,"first: data.subscriptions.0.subscription.amount_used; second: data.subscriptions.1.subscription.amount_used"),"first 31593277.00 · second —");
    assert_eq!(text(&one,&format!("all: data.subscriptions.0.subscription.amount_used + data.subscriptions.1.subscription.amount_used; used: {used}")),"all — · used 63.2%");
    for path in [
        "data.subscriptions.*.subscription.amount_used",
        "data.subscriptions[subscription.status=active].subscription.amount_used / 2",
        "data.subscriptions.*.subscription.amount_used %",
        "max(data.all_subscriptions.*.subscription.amount_used)",
        week,
        "sum(data.subscriptions.*.subscription.amount_used",
        "sum(data.subscriptions[status.amount_used)",
        "sum(data.subscriptions[status].amount_used)",
    ] {
        assert!(read(&one, path).is_err(), "{path}");
    }
    assert!(read(
        &one,
        &format!("week: {week}; second: data.subscriptions.1.subscription.amount_used")
    )
    .is_err());
    assert!(read(&two,"sum(data.subscriptions.*.subscription.amount_used + data.subscriptions[subscription.status=active].subscription.amount_total)").is_err());
    let parts = read(&one, &format!("used: {used}; week: {week}")).unwrap();
    assert!(parts[1]
        .error
        .as_ref()
        .unwrap()
        .contains("nothing to take it of"));
}
#[test]
fn extra_filter_chaining_count_strings_and_limits() {
    let reply = json!({"items":[{"plan":{"kind":"paid"},"on":true,"number":1.0,"name":"one","value":2},{"plan":{"kind":"free"},"on":true,"number":2,"name":"two","value":3},null]});
    assert_eq!(
        text(&reply, "sum(items[plan.kind!=free][on=true].value)"),
        "2.00"
    );
    assert_eq!(text(&reply, "count(items.*.name)"), "2.00");
    assert_eq!(text(&reply, "sum(items[number=1].value)"), "2.00");
    assert!(read(&reply, &format!("{}1{}", "(".repeat(100), ")".repeat(100))).is_err());
}
