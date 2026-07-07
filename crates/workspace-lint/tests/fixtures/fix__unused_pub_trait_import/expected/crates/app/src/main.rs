use demo::date::Date;
use demo::list::List;

fn main() {
    let l: List = [Date(2024001), Date(2023001)].into_iter().collect();
    let _n = l.dates().len();
}
