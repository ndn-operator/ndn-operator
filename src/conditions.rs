use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition as K8sCondition;
pub use operator_derive::Conditions as DeriveConditions;

// A trait for types that expose a Kubernetes-style `conditions` field
pub trait Conditions {
    // Accessors for the underlying conditions vector
    fn conditions(&self) -> &Option<Vec<K8sCondition>>;
    fn conditions_mut(&mut self) -> &mut Option<Vec<K8sCondition>>;

    // Default ergonomics: upsert a boolean condition into `self.conditions`
    fn upsert_bool(
        &mut self,
        type_: &str,
        status: bool,
        reason: &str,
        message: Option<&str>,
        observed_generation: i64,
    ) {
        let cond = self.make_condition(
            type_,
            status,
            reason,
            message.unwrap_or(""),
            observed_generation,
        );
        self.upsert_condition(cond);
    }

    // Clear all conditions
    fn clear_conditions(&mut self) {
        *self.conditions_mut() = None;
    }

    // Create a Kubernetes meta/v1 Condition with common fields populated
    fn make_condition(
        &self,
        type_: &str,
        status: bool,
        reason: &str,
        message: &str,
        observed_generation: i64,
    ) -> K8sCondition {
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
        let now = Time(chrono::Utc::now());
        K8sCondition {
            type_: type_.to_string(),
            status: if status {
                "True".to_string()
            } else {
                "False".to_string()
            },
            reason: reason.to_string(),
            message: message.to_string(),
            observed_generation: Some(observed_generation),
            last_transition_time: now,
        }
    }

    // Insert or update a condition in-place, preserving last_transition_time if status is unchanged
    fn upsert_condition(&mut self, new_cond: K8sCondition) {
        let target = self.conditions_mut();
        match target {
            Some(vec) => {
                if let Some(pos) = vec.iter().position(|c| c.type_ == new_cond.type_) {
                    if vec[pos].status != new_cond.status {
                        vec[pos] = new_cond;
                    } else {
                        let last_transition_time = vec[pos].last_transition_time.clone();
                        vec[pos].reason = new_cond.reason;
                        vec[pos].message = new_cond.message;
                        vec[pos].observed_generation = new_cond.observed_generation;
                        vec[pos].last_transition_time = last_transition_time;
                    }
                } else {
                    vec.push(new_cond);
                }
            }
            None => {
                *target = Some(vec![new_cond]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[derive(Default)]
    struct DummyConditions {
        conds: Option<Vec<K8sCondition>>,
    }

    impl Conditions for DummyConditions {
        fn conditions(&self) -> &Option<Vec<K8sCondition>> {
            &self.conds
        }

        fn conditions_mut(&mut self) -> &mut Option<Vec<K8sCondition>> {
            &mut self.conds
        }
    }

    #[test]
    fn make_condition_populates_core_fields() {
        let dummy = DummyConditions::default();
        let cond = dummy.make_condition("Ready", true, "Reason", "Message", 7);
        assert_eq!(cond.type_, "Ready");
        assert_eq!(cond.status, "True");
        assert_eq!(cond.reason, "Reason");
        assert_eq!(cond.message, "Message");
        assert_eq!(cond.observed_generation, Some(7));
    }

    #[test]
    fn upsert_condition_preserves_transition_time_on_same_status() {
        let mut dummy = DummyConditions::default();
        let original_time = k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(Utc::now());
        dummy.conds = Some(vec![K8sCondition {
            type_: "Ready".into(),
            status: "True".into(),
            reason: "Initial".into(),
            message: "Initial".into(),
            observed_generation: Some(1),
            last_transition_time: original_time.clone(),
        }]);

        let updated = K8sCondition {
            type_: "Ready".into(),
            status: "True".into(),
            reason: "Updated".into(),
            message: "Updated".into(),
            observed_generation: Some(2),
            last_transition_time: k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(Utc::now()),
        };
        dummy.upsert_condition(updated);

        let cond = dummy.conditions().as_ref().unwrap().first().unwrap();
        assert_eq!(cond.reason, "Updated");
        assert_eq!(cond.message, "Updated");
        assert_eq!(cond.observed_generation, Some(2));
        assert_eq!(cond.last_transition_time.0, original_time.0);
    }

    #[test]
    fn upsert_bool_and_clear_conditions_workflow() {
        let mut dummy = DummyConditions::default();
        dummy.upsert_bool("Ready", true, "Init", Some("ok"), 1);
        dummy.upsert_bool("Ready", false, "Fail", Some("not ok"), 2);

        let cond = dummy.conditions().as_ref().unwrap().first().unwrap();
        assert_eq!(cond.status, "False");
        assert_eq!(cond.reason, "Fail");
        assert_eq!(cond.observed_generation, Some(2));

        dummy.clear_conditions();
        assert!(dummy.conditions().is_none());
    }
}
