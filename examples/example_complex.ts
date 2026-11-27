type Count = number;

function identity<T>(value: T): T {
  return value;
}

function add(lhs: Count, rhs: Count): Count {
  return lhs + rhs;
}

identity(add(5, 7));
