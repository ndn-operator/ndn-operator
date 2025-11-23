use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;

pub mod conditions {
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
    pub trait Conditions {
        fn conditions(&self) -> &Option<Vec<Condition>>;
        fn conditions_mut(&mut self) -> &mut Option<Vec<Condition>>;
    }
}

#[derive(Default, operator_derive::Conditions)]
pub struct MissingFieldStatus {
    pub entries: Vec<Condition>,
}

fn main() {}
