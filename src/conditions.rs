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
